```
██╗  ██╗ ██████╗ ████████╗██╗     ██╗███╗   ██╗██████╗ ███████╗
██║ ██╔╝██╔═══██╗╚══██╔══╝██║     ██║████╗  ██║██╔══██╗██╔════╝
█████╔╝ ██║   ██║   ██║   ██║     ██║██╔██╗ ██║██████╔╝███████╗
██╔═██╗ ██║   ██║   ██║   ██║     ██║██║╚██╗██║██╔══██╗╚════██║
██║  ██╗╚██████╔╝   ██║   ███████╗██║██║ ╚████║██║  ██║███████║
╚═╝  ╚═╝ ╚═════╝    ╚═╝   ╚══════╝╚═╝╚═╝  ╚═══╝╚═╝  ╚═╝╚══════╝
```

[![CI](https://github.com/MenkeTechnologies/kotlinrs/actions/workflows/ci.yml/badge.svg)](https://github.com/MenkeTechnologies/kotlinrs/actions/workflows/ci.yml)
![Rust](https://img.shields.io/badge/Rust-2021-05d9e8?style=flat-square)
[![Docs](https://img.shields.io/badge/docs-online-blue.svg)](https://menketechnologies.github.io/kotlinrs/)
![license](https://img.shields.io/badge/license-MIT-ff2a6d?style=flat-square)
![status](https://img.shields.io/badge/status-active%20%C2%B7%20in%20development-9b5de5?style=flat-square)

### `[KOTLIN, COMPILED TO BYTECODE — JIT-COMPILED, NO JVM]`

> *"The JVM warms up. kotlinrs compiles and runs."*

**Kotlin in Rust** — a compiled Kotlin runtime, hosted on the
[`fusevm`](https://github.com/MenkeTechnologies/fusevm) bytecode VM with a
three-tier Cranelift JIT — the same engine behind `zshrs`, `strykelang`,
`awkrs`, `vimlrs`, `elisprs`, `rubylang`, `phplang`, `pythonrs`, and `node-js`.
No JVM, no `kotlinc`, no `.class` files.

### [`Read the Docs`](https://menketechnologies.github.io/kotlinrs/) &middot; [`Engineering Report`](https://menketechnologies.github.io/kotlinrs/report.html)

---

## Table of Contents

- [\[0x00\] Overview](#0x00-overview)
- [\[0x01\] Install](#0x01-install)
- [\[0x02\] Usage](#0x02-usage)
- [\[0x03\] Language Features](#0x03-language-features)
- [\[0x04\] Command-Line Flags](#0x04-command-line-flags)
- [\[0x05\] Architecture](#0x05-architecture)
- [\[0x06\] Status & Roadmap](#0x06-status--roadmap)
- [\[0xFF\] License](#0xff-license)

---

## [0x00] OVERVIEW

The reference Kotlin toolchain compiles to JVM bytecode and runs on a warm-up
JIT inside the JVM. `kotlinrs` lexes and parses Kotlin to an AST, lowers it to
`fusevm` bytecode, and runs it on a compiled VM with a Cranelift JIT — no JVM in
the loop. kotlinrs carries no VM or JIT of its own. Highlights:

- **Compiled, not tree-walked** — arithmetic, comparison, and control flow lower
  to native fusevm ops so the JIT can block- and trace-compile hot loops.
- **fusevm-hosted** — no local `vm.rs` / `jit.rs`; the shared engine behind
  `zshrs`, `strykelang`, `awkrs`, `vimlrs`, `elisprs`, `rubylang`, `phplang`,
  `pythonrs`, and `node-js`.
- **Native locals & calls** — `val`/`var` bindings compile to frame slots and
  `fun` calls to fusevm's native `Op::Call` sub-dispatch, with real recursion.
- **Kotlin-faithful boundaries** — a small extension handler supplies the
  behaviors the language-agnostic VM can't: Kotlin `toString()` for
  `Boolean`/`Double`, truncating integer `/` and `%` with an
  `ArithmeticException` on a zero divisor, the object heap behind classes /
  collections / lambdas, and the in-flight exception a VM with no unwind opcode
  cannot carry itself.

`kotlinrs` is an **M0 scaffold**: a genuinely running Kotlin subset (below), not
a stub. See the roadmap for what is next.

## [0x01] INSTALL

```sh
git clone https://github.com/MenkeTechnologies/kotlinrs
cd kotlinrs
cargo build --release
# the binary is target/release/kotlin
```

Requires a stable Rust toolchain. The only dependency is `fusevm` (which pulls
Cranelift for the JIT); everything else is std.

## [0x02] USAGE

```sh
# run a file
kotlin examples/fizzbuzz.kt

# one-liner (wrapped in `fun main` automatically)
kotlin -e 'println("2 + 2 = ${2 + 2}")'

# introspection
kotlin --dump-tokens   examples/hello.kt
kotlin --dump-ast      examples/hello.kt
kotlin --dump-bytecode examples/hello.kt
```

```kotlin
// examples/fib.kt
fun fib(n: Int): Int {
    return if (n < 2) n else fib(n - 1) + fib(n - 2)
}

fun main() {
    for (i in 0..10) {
        println("fib($i) = ${fib(i)}")
    }
}
```

## [0x03] LANGUAGE FEATURES

The M0 subset, all lowered to fusevm bytecode and exercised by the test suite:

- **Types** — `Int`/`Long` (`i64`), `Double`/`Float` (`f64`), `Boolean`,
  `Char` (a distinct runtime type — see below), `String`, `Unit`; annotations
  optional (including nullable `T?`), coarsely inferred otherwise.
- **Declarations** — top-level `fun` with typed parameters and return type,
  block bodies **and** single-expression bodies (`fun f(...) = expr`);
  `val`/`var` locals (`val` reassignment is a compile error, matching Kotlin);
  `fun main()` entry (with or without `args`).
- **Expressions** — `+ - * / %`, unary `-`/`!`, comparisons `== != < > <= >=`,
  short-circuit `&&`/`||`, parentheses. `Int/Int` truncates toward zero;
  `Double` division is IEEE. The bitwise operations are Kotlin's infix member
  functions — `and`, `or`, `xor`, `shl`, `shr`, `ushr` and `inv()`.
- **Literals** — decimal, hexadecimal (`0xFF`) and binary (`0b1010`) integers,
  each accepting `_` separators and an `L` suffix; and the companion constants
  `Int.MAX_VALUE`/`MIN_VALUE` (likewise `Long`/`Short`/`Byte`),
  `Double.MAX_VALUE`, and `Double`/`Float` `POSITIVE_INFINITY`/
  `NEGATIVE_INFINITY`/`NaN`.
- **Strings** — literals with `\n`/`\t`/`\\`/`\"`/`\$` escapes and `$name` /
  `${expr}` templates; `+` concatenates when either side is a `String`.
- **Call arguments** — positional and named (`f(count = 3)`, `p.copy(y = 2)`)
  for user functions, constructors and the `data class` `copy`, with Kotlin's
  rules enforced: positional arguments come first and each name binds a distinct
  parameter exactly once.
- **`Char`** — a runtime type of its own, not an `Int` in disguise, so it stays
  a character in *every* position, including the ones where the compiler can see
  no type: `println(listOf('a'))` is `[a]`, a `Map` key prints as `{a=1}`, and
  `x is Char` and `x is Int` answer differently. `'A'` literals (with
  `\n`/`\t`/`\uXXXX`/… escapes); Kotlin's `Char` operators — `'A' + 1` → `Char`,
  `'D' - 'A'` → `Int`, ordering and `==` by code unit — which hold inside a
  lambda (`listOf('a').map { it + 1 }` is `[b]`) as well as in typed code;
  `.code` (→ `Int`) and `Int.toChar()` (→ `Char`); `s[i]` and `for (c in s)`
  (both indexed by UTF-16 code unit, an out-of-range index a
  `StringIndexOutOfBoundsException`); `CharRange` (`'a'..'e'`, `step`, `downTo`,
  `reversed()`, `in`, iteration, and the `a..e` printed form); and the members
  `isDigit`/`isLetter`/`isLetterOrDigit`/`isWhitespace`/`isUpperCase`/
  `isLowerCase`/`uppercaseChar`/`lowercaseChar`/`uppercase`/`lowercase`/
  `digitToInt`/`compareTo`/`equals`/`hashCode`/`toString`.
- **Member access** — chainable postfix `.`: `String.length`,
  `.uppercase()`/`.lowercase()`, `.trim()`/`.trimStart()`/`.trimEnd()`,
  `.isEmpty()`/`.isNotEmpty()`, `.split()` (one delimiter or several),
  `.lines()`, `.reversed()`, `.take()`/`.drop()`, `.first()`/`.last()`,
  `.padStart()`/`.padEnd()`, `.substringBefore()`/`.substringAfter()`,
  `.removePrefix()`/`.removeSuffix()`, `.toCharArray()`, `.replaceFirst()`,
  the searching members in their overloaded spellings —
  `.indexOf(s, startIndex)`, `.lastIndexOf(s, startIndex)` (whose default
  `startIndex` is Kotlin's `lastIndex`, not Java's `length`, which shows for an
  empty needle), and `.startsWith(prefix, startIndex)` —
  `.compareTo()` (the JVM's code-unit difference, not a clamped sign) and
  `.compareTo(other, ignoreCase)`/`.equals(other, ignoreCase)`, the
  parses `.toInt()`/`.toLong()`/`.toDouble()` and their `…OrNull` forms, and
  `.format(args…)` — the `java.util.Formatter` conversions `%d %s %f %e %x %X
  %o %c %b %%` with the `-`/`0`/`+`/space flags, a width and a precision, where
  `%f` rounds HALF_UP over the value's shortest decimal form exactly as the JVM
  does (so `"%.0f".format(2.5)` is `3`, not `2`). Numerically,
  `.coerceIn()`/`.coerceAtLeast()`/`.coerceAtMost()`, `.pow()`,
  `.absoluteValue`, `.roundToInt()`; plus `Char.code`, `Int.toChar()`, and
  `Any.toString()`.
- **Classes** — `class C(val x: Int, var y: Int) { fun m() {…} }`: primary-
  constructor properties (`val`/`var`), plain constructor parameters (a
  parameter with no `val`/`var` is forwarded, not stored), instance methods
  (dispatched as native fusevm `Op::Call`s with an implicit `this`), property
  get/set (`p.x`, `p.y = …`), and implicit-`this` member access inside methods.
  `val`-property reassignment is a compile error.
  Properties may also be declared in the class **body**
  (`class C(val a: Int) { var c = 0; val d = a * 2 }`): each initializer runs per
  instance, after the superclass constructor and in declaration order, and may
  name a constructor parameter or an earlier body property. A `data class`'s
  generated members deliberately do not see them — Kotlin derives
  `toString`/`equals`/`hashCode`/`componentN` from the primary constructor alone,
  so `data class D(val a: Int) { val b = 2 }` prints `D(a=1)` while `D(1).b`
  still reads `2`.
  A **`companion object`** (one per class, named or not) is hoisted to a
  singleton reached through the class name — `C.K`, `C.of(…)` — and, from inside
  the class, with no qualifier at all.
  A property may be **computed** instead of stored — `val label: String get() =
  "$name($code)"`, with an `= expr` or a block getter. It has no backing field,
  so it runs on every read, and it dispatches virtually like any other member,
  which is what lets an implementor satisfy a declared property that way.
  A property may also be **declared without storage**: `val name: String` in an
  `interface`, or `abstract val` in a class. The declaration reserves the name
  and type — so a receiver of that type reads it, and an inherited default
  method's body may name it — while the implementor supplies the storage, with
  either `override val name: String` in its constructor or a getter.
- **`enum class`** — `enum class Color(val rgb: Int) { RED(0xFF0000), GREEN(…);
  fun hex() = … }`. Constants are singletons carrying `name` and `ordinal`, with
  `Color.values()` (a fresh array per call, as on the JVM), `Color.entries`, and
  `Color.valueOf(s)` — which throws `IllegalArgumentException("No enum constant
  Color.X")` for an unknown name, with the JVM's exact message. An enum's
  `toString()` is its constant's name, its equality is identity (so a `when (c)`
  arm and a `Set`/`Map` behave), and it is `Comparable` by *declaration* order,
  so `sorted()` restores the order the constants were written in and
  `compareTo` answers the ordinal difference the JVM's does. A constant may
  carry its own **body** (`PLUS("+") { override fun apply(a, b) = a + b }`),
  which makes it an anonymous subclass — the shape that lets the enum declare an
  `abstract fun` each constant implements. An enum may implement an interface
  and declare a `companion object` of its own.
- **Inheritance** — `open class`/`abstract class`/`sealed class`/`interface`,
  the `: Super(args), Iface1, Iface2` supertype list, `override`, `abstract`
  members with no body, interface members *with* a default body, and
  `super.m()` — plus the qualified `super<T>.m()`, which names *which* supertype
  to run and is what Kotlin requires when two of them implement `m`
  (`super<Left>.pick() + super<Right>.pick()`); a `T` that is not a direct
  supertype, or that does not implement `m`, is a compile error, as in Kotlin.
  Dispatch is **virtual**: a call resolves
  against the receiver's *runtime* class, so `val a: Animal = Dog(…)` runs
  `Dog`'s override, and a base-class method calling an overridden one lands in
  the override too. Fields are flattened base-most first and a subclass's
  constructor chains to its superclass's, so `class Dog(name: String) :
  Animal(name)` forwards. `is` is a full expression (`x is Dog`, `x !is Cat`),
  not only a `when` arm, and it answers for every level of the hierarchy —
  including an `interface`.
  A user class may extend a **built-in throwable** (`class ParseError(m: String)
  : IllegalArgumentException(m)`): it carries `.message`, is claimed by
  `catch (e: IllegalArgumentException)` / `catch (e: Exception)` on the real
  hierarchy, and prints as `ParseError: m` rather than the identity form.
  A `toString()` override is honoured wherever a value is rendered — `println(x)`,
  a template, `x.toString()`, and *inside* a printed `List`/`Map`/`Pair` or a
  `joinToString`.
  A single-implementation call stays a direct `Op::Call`; only a genuinely
  polymorphic site pays for a runtime class-tag test, and a program with no
  supertype anywhere emits the bytecode it did before.
- **`data class`** — auto-generated `equals`/`hashCode` (structural),
  `toString()` (`C(x=1, y=2)`), `copy(...)` (positional overrides), and
  `componentN`, so `val (a, b) = p` destructures. `==` on a data class /
  collection is structural. A `data class` **may inherit stored properties**
  (`data class Leaf(val v: Int) : Node(1)`): Kotlin derives the members from the
  primary constructor *alone*, so the inherited field is readable (`leaf.depth`)
  but is not part of `toString`/`equals`/`hashCode`/`componentN`, and `copy`
  calls the primary constructor — re-running the `: Super(args)` header, which
  is what makes `data class W(val s: String) : Base(s.length)` recompute its base
  field on a copy rather than carry the old one over.

  `hashCode()` follows the **JVM contract exactly**, so the number a program
  prints matches the reference toolchain rather than merely being consistent
  within a run: `Int` hashes to itself and `Long` folds its two halves
  (`(-1).hashCode()` is `-1`, `(-1L).hashCode()` is `0` — the width the compiler
  saw travels with the call, and a `data class`'s `Long` field is recorded in its
  class metadata), `Double` folds `doubleToLongBits`, `Boolean` is `1231`/`1237`,
  `String` is the `31`-polynomial over UTF-16 units, a `List` folds `31`, a `Set`
  and a `Map` SUM (so insertion order cannot matter), a `Map.Entry` is
  `key xor value`, an `IntRange` is `31 * first + last` and an `IntProgression`
  adds its step. The identity-hashed kinds — a non-`data` class instance, an
  array, a lambda — answer their heap handle, which is what the JVM does too
  (and is why no such value is fuzzed or frozen).
- **`Map.Entry`** — a distinct type from `Pair`, because all three observable
  members differ: an entry renders `k=v` where a pair renders `(k, v)`, its hash
  is `key xor value` where a pair folds like the `data class` it is, and
  `mapOf(1 to "a").entries.first() == (1 to "a")` is `false`. `keys` and
  `entries` are `Set`s, so their hash sums and their equality ignores order;
  `values` is a plain `Collection`, whose `equals`/`hashCode` the JVM leaves as
  identity.
- **`object`** — singleton declarations with `val`/`var` properties and methods,
  built once and reachable by name (`Counter.inc()`). An `object` may
  declare supertypes (`object Registry : Greeter`), which its own methods and
  `is` checks answer for.
- **Collections** — `listOf`/`mutableListOf`, `setOf`/`mutableSetOf`, and
  `mapOf`/`mutableMapOf` (with `k to v` `Pair`s), indexing `xs[i]` / `m[k]` (and
  indexed assignment), `.size`, `.add`/`.remove`/`.get`/`.contains`/`.indexOf`/
  `.sum` on lists, `.containsKey`/`.keys`/`.values`/`.entries`/`.put` on maps.
  `List`s, `Set`s, arrays, and ranges share one sequence-member table:
  `.count()`, `.first()`/`.last()`, `.max()`/`.min()`, `.average()`,
  `.toList()`/`.toSet()`, `.distinct()`, `.sorted()`/`.sortedDescending()`,
  `.take(n)`/`.drop(n)` (both clamp rather than fault), `.flatten()`,
  `.zip(other)`, `.chunked(n)`/`.windowed(n, step, partialWindows)`,
  `.subList(from, to)`, `.slice(indices)`,
  `.union`/`.intersect`/`.subtract`,
  `.joinToString(sep, prefix, postfix, limit, truncated)`, `.reversed()`. The
  `…OrNull` members answer `null` where their plain counterparts throw:
  `.maxOrNull()`/`.minOrNull()`, `.firstOrNull()`/`.lastOrNull()`,
  `.getOrNull(i)`/`.elementAtOrNull(i)` beside `.elementAt(i)`.
- **`Set`** — `setOf`/`mutableSetOf` build a `LinkedHashSet`, so iteration and
  display follow *insertion* order (`setOf(3, 1, 2, 3)` prints `[3, 1, 2]`) while
  equality ignores it (`setOf(1, 2) == setOf(2, 1)`). `MutableSet.add` answers
  whether the element was new, where `MutableList.add` always answers `true`.
  A `Set` iterates, indexes into the sequence members, and feeds the
  higher-order functions like any other `Iterable`.
- **Ranges** — first-class values, not just `for` headers: `1..5`,
  `1 until 5`, `5 downTo 1`, `… step n`, bound to names, printed
  (`IntRange` shows `1..5`, `IntProgression` shows `1..9 step 2` — its last
  *reachable* element), aggregated (`(1..3).sum()`), mapped/filtered, and
  iterated. `x in r` / `x !in r` is step-aligned membership; `in` also works over
  a `List`, a `Map`'s keys, and a `String`'s substrings.
- **Arrays** — `arrayOf`/`intArrayOf`/`doubleArrayOf`/`booleanArrayOf`, the
  zero-filled `IntArray(n)`/`DoubleArray(n)`/`BooleanArray(n)`, and the
  index-lambda initializers `IntArray(n) { it * 2 }` / `DoubleArray(n) { … }` /
  `Array(n) { … }`, with `[i]` read
  and write, `.size`, the shared sequence members, and `for (x in a)`. An array
  keeps JVM semantics: `==` is reference identity (`arrayOf(1) == arrayOf(1)` is
  `false`) and `toString()` is `[I@…`-style (the identity-hash digits are ours,
  the shape is Kotlin's).
- **Math** — `kotlin.math` `abs`/`max`/`min`/`sqrt`/`floor`/`ceil`/`round` and
  `PI`/`E`, gated on `import kotlin.math.*` (or a single-name import, honouring
  `as` renames) exactly as Kotlin gates them; the auto-imported `maxOf`/`minOf`;
  and the `java.lang.Math` statics, which need no import. `round` and
  `Math.round` differ as they do in Kotlin — half-to-even returning `Double`
  versus half-up returning `Long`.
- **First-class lambdas** — `{ it * 2 }`, `{ a, b -> a + b }`, function-type
  values (`val f: (Int) -> Int = …`) and parameters (`fun apply(f: (Int) -> Int, x: Int) = f(x)`);
  store, pass, return, and invoke (`f(3)`); trailing-lambda call syntax; captures
  the enclosing scope by value (a returned lambda keeps its upvalues). Each
  lambda is a heap closure object invoked through a re-entrant VM run — no
  compiler inlining and no fusevm-core change.
- **Higher-order stdlib** — the lambda-taking collection functions operate on
  real lambda values: `.map`/`.mapIndexed`/`.flatMap`, `.filter`/`.filterNot`,
  `.partition`, `.takeWhile`/`.dropWhile`, `.firstOrNull`/`.lastOrNull`,
  `.forEach`, `.fold`/`.reduce`, `.any`/`.all`/`.none`/`.count`, `.sumOf`,
  `.maxByOrNull`/`.minByOrNull`, `.sortedBy`/`.sortedByDescending`,
  `.associate`/`.associateBy`/`.associateWith`, `.groupBy`,
  `.first`/`.last`/`.find`/`.findLast`/`.single` and their `…OrNull` forms,
  `.indexOfFirst`/`.indexOfLast`, `.filterIndexed`/`.forEachIndexed`,
  `.maxOf`/`.minOf`, `.mapValues`/`.mapKeys`, `.getOrElse(i) { }`,
  `.sortedWith { a, b -> … }`, and the transform-taking overloads of
  `.joinToString`, `.chunked`, `.windowed`, and `.zip`. `it` is the implicit
  parameter, and `mapIndexed` takes `(index, element)`. A `Map` receiver feeds
  them one `Map.Entry` per element (`m.map { it.key }`), and `filter` re-wraps
  into the receiver's own kind — a filtered `Map` is a `Map`, a filtered `Set` a
  `Set` — as Kotlin's per-receiver overloads do.
- **Scope functions** — both families, on any receiver. The `it`-form
  `.let`/`.also`/`.takeIf`/`.takeUnless` passes the receiver as the lambda's
  parameter; the `this`-form `.run`/`.apply` and the free `with(x) { … }` bind it
  as the block's **`this`**, so the receiver's members are reachable with no
  qualifier (`"abc".run { length }`, `Box(2, 3).apply { w = 5 }`). `run`/`let`
  yield the block, `apply`/`also` the receiver. The receiverless `run { … }` is a
  block evaluated on the spot for its value. The receiver's declared type reaches
  the block, so an `Int` receiver's arithmetic still wraps at 32 bits inside it.
- **Extension functions** — `fun Int.dbl(): Int = this * 2`,
  `fun String.shout() = uppercase() + "!"`, `fun Person.label() = name`, with
  defaults and `vararg` like any other function. Dispatch is by the receiver's
  **static** type, which is what keeps an `Int` and a `Long` extension of one
  name apart (they share a runtime representation) and what makes the `Int` one's
  arithmetic wrap at 32 bits. A member function of the same name and arity wins,
  as in Kotlin; inside the body `this` is the receiver, an unqualified name is a
  member of it, and a user-class receiver's properties are in scope. A receiver
  the frontend cannot type falls back to a sole program-wide extension of that
  name, and an ambiguous one is a compile error rather than an arbitrary pick.
- **Default, named and `vararg` parameters** — `fun f(a: Int, b: Int = 10)`,
  `f(b = 3, a = 4)`, `fun total(vararg xs: Int)`, for functions, methods,
  constructors and extensions alike. Defaults are evaluated at the CALL site, so
  one may not name another parameter of the same callee; that form is rejected
  rather than silently misbound. A `vararg` binds an array of its declared
  element type and is supported as the last parameter.
- **Local functions** — a `fun` declared inside another function's body. It
  lowers to a real subroutine rather than a closure value, which is what lets it
  **recurse** (a closure captures by value at creation, so a self-reference would
  read an uninitialized slot). It takes defaults, shadows a top-level function of
  its name for the rest of the enclosing body, and is callable from a lambda
  there. It cannot close over the enclosing frame's locals — naming one is an
  unresolved reference, not a wrong answer.
- **`Pair` / `Triple`** — the constructor spellings beside `a to b`, with the
  `data class` behaviour Kotlin gives them: `(a, b)` / `(a, b, c)` display,
  structural equality, the `31`-fold `hashCode`, `first`/`second`/`third`, and
  `componentN` so `val (a, b, c) = t` destructures.
- **Captured `var` mutation** — a `var` of the enclosing frame that a lambda
  *assigns* to is stored in a shared cell, so the write is visible to the frame
  (`var n = 0; xs.forEach { n += it }`). This is what the JVM backend does with
  its `Ref.IntRef` wrappers, and the boxed value keeps its declared width, so an
  `Int` accumulation still wraps at 32 bits.
- **Casts** — `x as T` and the safe `x as? T`. The runtime value is unchanged;
  what the cast supplies is the **static** type, which then decides integer width
  and `/` dispatch. A mismatch throws `ClassCastException`, where `as?` is
  `null` — and a null `as? String` prints as the four characters `null`. `Int`
  and `Long` are one runtime representation here, so a cast cannot tell them
  apart (see the limitation note below).
- **Top-level properties and `by lazy`** — `val K = 7` / `var counter = 0` at
  file scope, initialized in declaration order before `main` and visible to every
  function; a local of the same name shadows one. `val z: Int by lazy { … }` —
  on a top-level property, a class property **or a local `val`** — runs its block
  at the **first read** and caches the result, so an initializer with an effect
  fires at use rather than at startup. `lazy` is the only supported delegate; any
  other `by` is a compile error, and on a local it is the only one accepted at
  all (a local has no property object to hand a `getValue` delegate).
- **`runCatching` / `Result`** — `runCatching { … }` runs a block and packages
  its outcome as `Success(v)` / `Failure(<throwable>)`, catching the runtime
  faults this frontend raises as well as an explicit `throw`. The readers are
  total: `isSuccess`/`isFailure`, `getOrNull()`/`exceptionOrNull()`,
  `getOrElse { }`, `map { }`, `onSuccess { }`/`onFailure { }`.
- **Control flow** — `if`/`else` (statement **and** expression, incl.
  `else if`); `when` (statement **and** expression) in subject and subjectless
  forms, with literal, comma-grouped, `in`/`!in` range, `is`/`!is` type (incl.
  the erased generic form `is List<*>`), and `else` arms; the subject may name
  itself for the arm bodies (`when (val n = f()) { … }`). `while`,
  `do { … } while (cond)` — whose body always runs once and whose `continue`
  targets the condition, not the loop top — and `for`, over a literal range
  (`a..b`, `a until b`, `a downTo b`, with optional `step`), which lowers to a
  counted native-op loop, or over any iterable value (a `List`, a `Map`, an
  array, a range held in a variable, or a `String` — `for (c in "abc")` walks its
  `Char`s), with the destructuring header `for ((k, v) in map)`.
  A loop body may be a block or a single statement;
  `break`/`continue`, including labeled `outer@ for (…)` with
  `break@outer` / `continue@outer`, and the local `return@label` out of a
  lambda. Blocks are lexically scoped: bindings
  declared in a nested block (and the `for` variable) drop at the block's end;
  shadowing is restored. A `when` over a `sealed` hierarchy's `is` arms needs no
  `else` — the arms cover every subtype, so the fallthrough is unreachable.
- **Null safety** — `null` literal and nullable types `T?`; safe call `?.`
  (short-circuits to null, and chains: `a?.b?.c`), Elvis `?:`, and the not-null
  assertion `!!` (throws `NullPointerException` on null). `x == null` / `x != null`
  are null tests, not value comparisons, and a null `String?` renders as the four
  characters `null` in a template or a `+` — so `"v=$n"` reads `v=null`.
- **Exceptions** — `try` / `catch` / `finally` / `throw`, with `try` as an
  **expression** (`val n = try { f() } catch (e: Exception) { -1 }`). `catch`
  arms are tested in source order against the JVM throwable hierarchy, so
  `catch (e: RuntimeException)` claims an `IllegalArgumentException`; `finally`
  runs on both the normal and the exceptional path, and an exception raised *by*
  a finalizer replaces the one it interrupted. The modeled throwables construct
  and print like the JVM's (`RuntimeException("boom")` →
  `java.lang.RuntimeException: boom`, `.message` → `boom` or `null`), and the
  runtime faults kotlinrs already reported are the *same* catchable exceptions:
  `1 / 0` is an `ArithmeticException`, `!!` on null a `NullPointerException`, an
  out-of-range index an `IndexOutOfBoundsException`. An uncaught exception
  reports `Exception in thread "main" <class>: <message>` on stderr and exits
  non-zero.
- **Functions** — user calls, recursion, `return`, `Unit` functions.
- **Increment / decrement** — `x++`, `x--`, `++x`, `--x` on a variable, a
  property, or an indexed element, in statement **and** expression position
  (`println(i++)` yields the pre-update value, `println(++i)` the post-update
  one).
- **Built-ins** — `println(...)` / `print(...)`.
- **Imports** — `package` and `import` declarations, including `a.b.*` and
  `a.b.c as d`, parsed and used for name resolution.
- **Type arguments** — explicit call type arguments (`listOf<Int>()`,
  `emptyMap<String, Int>()`) are parsed and ignored; typing stays coarse, and
  `a < b` still parses as a comparison (a type-argument list may hold only names
  and must be followed by `(`, which is how Kotlin resolves the same ambiguity).
- **Comments** — `//` and nested `/* … */`.

Generic *declarations* carry no type variable in the coarse type itself, but a
**use site supplies the one it needs**. Kotlin's integer width is a property of
the static type, so a `T` that reads as untyped silently changes the answer:
`gid(2_000_000_000) + gid(2_000_000_000)` has to wrap at 32 bits, and
`gid(2_000_000_000L) + gid(2_000_000_000L)` has to not. Two sources are
resolved:

- **An argument**, for `fun <T> id(x: T): T` — the type argument is whatever the
  matching argument's type is.
- **The receiver's type argument**, for a member of a generic class. A
  construction fixes it (`Box(65536)` makes `T` an `Int`), and a read of a
  `T`-typed stored property, `var` property, computed property (`val v: T get()
  = …`) or method result (`fun get(): T`) answers with it. Nested
  instantiations (`Box(Box(65536)).v.v`) and classes with several type
  parameters (`Pair2<A, B>`) resolve per position.

Everything the frontend cannot resolve stays untyped, which narrows nothing —
so a `String` or `Double` type argument is never mistaken for an `Int`. Type
arguments written down rather than inferred are still discarded: `val b:
Box<Int> = …` and `fun f(): Box<Int>` reach their members untyped unless the
construction is in hand.

The `inline` / `noinline` / `crossinline` / `tailrec` / `operator` / `infix` /
`const` modifiers are accepted and discarded — each changes how the JVM compiles
a declaration, not what it computes. A **reified** type test is the one case
that cannot be erased quietly: `x is T` / `x as T` against a type parameter has
no answer a coarse type system could give, and answering `false` (the shape a
name-based lookup falls into) would be silently wrong — so it is a compile error.

Not yet (see roadmap): `sequence { … }` / `yield` (a generator needs a
continuation this VM has no opcode for, so an infinite sequence cannot be
modelled by evaluating eagerly), a type argument written down rather than
inferred from a construction (`val b: Box<Int> = …`, `class Sub : Box<Int>()`),
variance and bounds,
`kotlin.properties.Delegates.observable` / `vetoable` (a property delegate that
is not a constructor call has no class whose `getValue` could be resolved at
compile time, and no host-side delegate object backs the stdlib factories yet),
an explicit `: super(args)` from a secondary constructor (the primary
constructor is the only thing that chains to the superclass here), calling a
method on `this` from inside an `init` block (the instance is allocated after
the initializers run, so there is no receiver yet — reading properties works),
and the rest of the standard-library surface. Each fails loudly rather than
answering wrong.

One limitation is a **representation** limit rather than a missing feature, and
so is excluded from the fuzzer and the frozen corpus by design: every integer is
one `i64` at runtime, so a cast that has to tell a boxed `Int` from a boxed
`Long` (`anyHoldingAnInt as Long`, which Kotlin rejects at run time) cannot be
checked. It is the same class of exclusion as `Map.values`, whose `equals` and
`hashCode` the JVM leaves as identity.

The inheritance **modifiers are enforced**, on the same four rules Kotlin
applies: a class may only extend a `class` marked `open`/`abstract`/`sealed`
(an `interface` is always implementable); `override` must have something to
override; a member that redeclares a supertype's must say `override`; and what
it overrides must be overridable — `open`, `abstract`, or itself an `override`,
with every `interface` member implicitly open. A member is matched by name *and*
arity, so a same-named member at another arity stays an overload.

**Instance equality is Kotlin's**, which means three different answers
depending on what a class declares, and every one of them is observable:

- A class that declares neither `data` nor `equals` inherits `Any.equals` —
  **reference identity**. `Plain(1) == Plain(1)` is `false`, `listOf(Plain(1))`
  does not contain `Plain(1)`, and two of them do not collapse in a `Set`.
- A `data class` compares its primary-constructor properties structurally.
- A declared `equals` wins over both, and it is reached from **every** place
  equality is: `==`/`!=`, `in`, `contains`/`containsAll`/`indexOf`/`remove`,
  `distinct`, `Set` membership, `Map` key lookup and `mapOf`'s repeated-key
  collapse, and recursively through a `List`/`Set`/`Map`/`Pair`/`Triple` that
  holds one. A declared `hashCode` likewise drives the container folds.

The two container families do **not** share a rule, and the difference is
visible exactly when a class supplies one half of the contract without the
other. A `List` compares with `equals` alone; a `Set`, a `Map` key and
`distinct` reach `equals` only once the hashes agree — so a class with `equals`
but no `hashCode` is found by `listOf(...).contains(...)` and still keeps its
duplicates in a `Set`. `Set` and `Map` *equality* is hash-gated too, because
`AbstractSet.equals` is `containsAll` and `AbstractMap.equals` is `get`.

`===` / `!==` ask the other question — whether both sides denote the **same
object** — and never reach an `equals` override at all. `listOf(1) == listOf(1)`
is `true` while `listOf(1) === listOf(1)` is `false`; `a === a` is `true` for
every value. Values this runtime does not box (numbers, `Char`, `Boolean`,
`String`, `null`) compare by value, which agrees with the JVM wherever a Kotlin
program can observe the answer without boxing: `1 === 1` and `"x" === "x"` are
`true`, the latter because the JVM interns literals. Two answers that *are*
artifacts of boxing are not modelled — an `Any`-typed integer outside the
`Integer` cache (`val a: Any = 1000; val b: Any = 1000; a === b` is `false` on
the JVM) and a `String` assembled at run time rather than folded at compile
time. Nothing else here models a box, so neither does this.

`==` itself does not short-circuit on identity: it lowers to
`Intrinsics.areEqual(a, b)`, so a declared `equals` runs even for `x == x`. A
hash container does short-circuit (`HashMap.getNode` tests `k == key` first), so
a lookup by the stored object skips the body. This is only observable through an
`equals` with an effect, and it is checked by the frozen corpus.

Method overloading is not supported — two `fun f` at different arities in one
class is a compile error here.

One equality corner is **excluded** rather than modelled, and the exclusion is
deliberate: a 1-element `setOf`/`mapOf` is `java.util.Collections.singleton` /
`singletonMap` on the JVM, whose `contains`/`get` consult `equals` **alone** —
no hash gate and no identity check — where every other size uses a
`LinkedHashSet`/`LinkedHashMap` and gates on the hash. So
`mapOf(e1 to 5)[e2]` finds the entry while `mapOf(e1 to 5, e3 to 6)[e2]` does
not, for a class with `equals` and no `hashCode`. Reproducing that needs the
container to remember whether it came from a 1-element immutable literal — a
provenance this runtime's `Set`/`Map` do not carry — and it only shows up for a
class that has already broken the `equals`/`hashCode` contract. The fuzzer keeps
its probes off 1-element immutable containers for this reason.

A `return`, `break` or `continue` out of a `try` that owns a `finally` is
honoured: the finalizer runs first, and every finalizer between the jump and its
target runs innermost-first. That includes a labeled `break@outer` from a loop
nested *inside* the `try`, which crosses the `try` without appearing in its body.
A `try` with neither a `catch` nor a `finally` is refused (Kotlin rejects it as
well).

`Int` and `Long` are both carried as a 64-bit integer at runtime, and the
difference is restored **per site** from the operands' static types: an `Int`
result is narrowed back to 32 bits by a `Shl 32; Shr 32` pair (fusevm's `Shr` is
an arithmetic shift, so that is a two's-complement sign-extend), and a `Long`
result is left alone. So `2147483647 + 1` is `-2147483648` and
`2147483647L + 1L` is `2147483648`, in the same program and the same chunk. The
narrowing is two native ops rather than a host call, which keeps a hot `Int`
loop traceable by the JIT, and it is a no-op for a value already in range.

It covers `+ - * / %`, unary minus, `++`/`--`, compound assignment, and
`abs(Int.MIN_VALUE)`. `Byte` and `Short` follow Kotlin in promoting to `Int`
before every operator, while `toByte()`/`toShort()`/`toInt()` truncate to their
own width and `Double.toInt()` saturates. The shifts take the receiver's width
too: `shl`/`shr`/`ushr` mask the count at 31 and truncate to 32 bits on an `Int`
receiver, at 63 and 64 bits on a `Long` one (`1 shl 32` is 1, `1L shl 32` is
4294967296), and `inv()` complements the matching width.

The primitive array factories name their element type, so an `IntArray` element
takes part in all of it — `ia[0] + 1` wraps and `ia[0] / 2` divides as integers,
where a `List` element (which could hold anything) does neither.

An unannotated lambda parameter is typed from its CALL SITE, not left unknown:
`listOf(1, 2).map { it * it }` types `it` from the receiver's elements, so the
product narrows the way Kotlin's does, while `listOf(1L).map { it * 4 }` keeps
64 bits. The element type is read from a collection or array literal, a range, a
`val` initialized from one, the members that re-emit their receiver's elements
(`filter`, `sorted`, `take`, …), and a declared function type
(`val f: (Int) -> Int = { … }`). It reaches `for` variables, the two-parameter
forms (`fold`'s accumulator, `reduce`, `mapIndexed`, `zip`, `sortedWith`), the
group a `chunked`/`windowed` lambda receives, and the members that hand back one
element (`first()`, `last()`, `xs[i]`).

What still is *not* narrowed is an operand whose static type this frontend
genuinely cannot resolve: the result of `map`/`flatMap` (its element type is the
lambda's return type), a sequence reached through a function return, an element
two levels down through a named binding, and a `Map` entry's `value`. Those keep
the 64-bit result, on the reasoning that silently truncating a value that may be
a `Long` is worse than an unwrapped overflow.

## [0x04] COMMAND-LINE FLAGS

| Flag | Effect |
|------|--------|
| `-e`, `--eval <src>` | Run a snippet (repeatable, newline-joined); wrapped in `fun main` if it has none. |
| `--dump-tokens` | Print the lexer token stream and exit. |
| `--dump-ast` | Print the parsed AST and exit. |
| `--dump-bytecode`, `--disasm` | Print the lowered fusevm chunk disassembly and exit. |
| `--tiers FILE` | Run it, then report which fusevm execution tier took each of its chunks. |
| `--lsp` | Speak the Language Server Protocol over stdio (diagnostics, completion, hover). |
| `--dap` | Speak the Debug Adapter Protocol over stdio (breakpoints, stepping, live locals). |
| `-v`, `--version` | Print the version and exit. |
| `-h`, `--help` | Print help and exit. |

An inline `rust { pub extern "C" fn … }` block inside a function body compiles
to a cached cdylib whose exported functions are callable by name from Kotlin
(via `fusevm::ffi`). Editor tooling (`--lsp`, `--dap`) and a generated
[reference page](https://menketechnologies.github.io/kotlinrs/reference.html)
share the language-server corpus, so they never drift.

## [0x05] ARCHITECTURE

```text
Kotlin source
   │  lexer.rs      → tokens (string templates pre-split)
   │  parser.rs     → AST (ast.rs)
   │  compiler.rs   → fusevm::Chunk   (native ops + Kotlin extension ops)
   ▼
fusevm::VM  ──►  three-tier Cranelift JIT (linear · block · tracing)
   ▲
   │  host.rs       → value coercions + object heap (classes, List/Map/Pair,
   │                  closures) + lambda builtins (make/call/HOF/scope)
```

- `compiler.rs` keeps one invariant: every expression leaves exactly one value
  on the stack and every statement is stack-neutral, so `if`/`while`/`for`/`when`
  balance without a separate analysis pass.
- The only Kotlin-specific runtime code is a set of extension ops in `host.rs`
  (value coercions, member dispatch, `is` checks, null tests), a few builtins
  (`Op::CallBuiltin`) for the lambda operations (make-closure, closure-call, the
  higher-order collection dispatch, scope functions), plus a **frontend-owned
  object heap**: a `Value::Obj(u32)` handle indexes a host-side table of class
  instances, lists, maps, pairs, and closures. A lambda is a heap closure (body
  chunk index + captured upvalues) invoked through a re-entrant `vm.run()` — the
  builtin dispatch keeps the extension handler live across that nested run, so
  host ops work inside a lambda body. fusevm just carries the handle
  (identity-comparable); the frontend owns the pointed-to object — the same model
  the other mature fusevm frontends use. Everything else is a universal fusevm op.
- **A `Char` that survives an untyped position.** `fusevm::Value` has no `Char`
  variant, and the obvious substitutes both fail: an `Int` code unit cannot be
  told apart from the number `97`, and a one-character `String` would make
  `it + 1` concatenate where Kotlin does `Char` arithmetic. A `Char` is instead
  the top 64 K of the `Value::Obj` handle space (`0xFFFF0000 | code`) — the
  handle *is* the character, so it allocates nothing, interns for free (two
  `'a'`s are the same handle, which keeps `==` a native integer compare and lets
  a char be a `Map`/`Set` key), and is disjoint from every `Int`. Because it is
  not a number, the native `Op::Add`/`Op::NumLt` that `'a' + 1` and `c < 'z'`
  lower to — even inside a lambda, where no static type is available — would
  coerce it. So the VM runs under fusevm's **strict numeric policy**: an operand
  fusevm cannot compute on is handed to a Kotlin `NumericHook` rather than
  guessed at, and the same switch keeps the JIT from compiling a block whose
  slots hold a char (which would reach native code as a `0`). Int/Int arithmetic
  stays on the native fast path throughout.
- **Exceptions without an unwind opcode.** fusevm has none, and kotlinrs lowers
  `fun`s to *native* `Op::Call` frames, so a `throw` cannot longjmp out of a
  frame. It is instead the two-part protocol the sibling frontends (`javars`,
  `scalars`) converged on: the host parks the throwable in a pending slot and
  suppresses every side-effecting builtin while it is in flight (so nothing
  prints between the raise and its handler), while the compiler emits a pending
  check after each statement that jumps to the innermost enclosing handler — out
  of a loop, out of a frame, into a `catch` dispatch, or into the terminal report
  at the end of `main`. Unwinding is therefore statement-granular: the abandoned
  statement finishes evaluating on garbage operands, which the handler discards
  by truncating the value stack to the depth recorded at `try` entry. A program
  with no `try`/`throw` emits **zero** extra ops and keeps the native print op,
  so it pays nothing for the feature.
- **A captured `var` lives in a cell.** A lambda is a heap closure that copies
  its upvalues at creation, so a write inside it could never reach the frame that
  declared the variable. Before lowering a body, the compiler collects every name
  a lambda anywhere inside it assigns to; a `var` declared there under one of
  those names is stored in a one-element heap cell instead, and every read and
  write goes through it. Both sides then hold the same handle, which is exactly
  what the JVM backend does with its `Ref.IntRef` wrappers. The analysis
  over-approximates on purpose — a name that turns out to be the lambda's own
  local costs one cell and nothing else — because under-approximating would mean
  a silently stale number.
- **Two differential harnesses, and neither may see our own output.** The live
  `parity-fuzz` binary generates programs and diffs them against a real
  `kotlinc` + `kotlin` pair; `tests/data/parity_expected.txt` freezes a curated
  corpus of the same programs with the output that toolchain gave, so CI can
  replay them with no JVM installed. `scripts/capture-parity.sh` is the only way
  records enter that file, and it invokes the reference toolchain **exclusively**
  — a corpus recorded from kotlinrs would be a record of our own behaviour rather
  than of Kotlin's, and would pass forever no matter what broke.
- **A run the oracle could not answer is not a pass.** Two failing sides compare
  equal, so a program `kotlinc` rejects — or a `kotlin` that times out — produces
  no output on either side and scores as agreement. `parity-fuzz` therefore
  requires the reference toolchain to have exited 0 with non-empty stdout before
  a program counts, reports the rest as **barren** with their probe count, and
  exits non-zero on them. Its summary prints probes *generated* and probes
  *compared* separately, so a gap between the two is visible rather than
  flattered. Scratch directories are per-process (pid plus a counter) in both
  harnesses, because several agents share this checkout and a fixed path lets one
  run delete another's case mid-iteration.

## [0x06] STATUS & ROADMAP

M0 (this release): the running language subset above, with a headless test
suite and `--dump-*` introspection.

Landed since M0: a host-side object heap backing classes, `data class`es,
`object` singletons, `List`/`Map`/`Pair` collections and their methods;
first-class lambda values (capture, store/pass/return/invoke, trailing-lambda
syntax) as heap closures; the lambda-taking higher-order collection functions
(`map`/`filter`/`forEach`/`fold`/`reduce`/`any`/`all`/`count`/`sumOf`/
`maxByOrNull`/`sortedBy`/`associateWith`/`groupBy`); the `it`-form scope
functions (`let`/`also`/`takeIf`); ranges as values (`1..5`, `until`, `downTo`,
`step`, `in`/`!in`) with the `IntRange`/`IntProgression` display split; arrays
(`arrayOf`, `IntArray(n)`, indexing, `.size`) with JVM reference equality;
`kotlin.math`/`java.lang.Math` under Kotlin's own import rules; `++`/`--` in
expression position; `try`/`catch`/`finally`/`throw` with `try` as an expression
and host faults raised as catchable JVM exceptions; the array index-lambda
initializers (`IntArray(n) { it * 2 }`, `Array(n) { … }`); `String` iteration
(`for (c in "abc")`); and the null-safety corners — `x == null` as a null test,
`String?` display, and the operator methods (`n?.plus(1)`).

Landed since then: **class inheritance** — `open`/`abstract`/`sealed` classes,
`interface`s with default members, the `: Super(args), Iface` supertype list,
`override`, `super.m()`, virtual dispatch by runtime class tag, `is` as a full
expression over the whole hierarchy, user classes extending the built-in
throwables (so `class ParseError(m: String) : IllegalArgumentException(m)` is
caught and printed like a JVM one), and a `toString()` override honoured through
nested collection rendering.

Also landed: `Set` (`setOf`/`mutableSetOf`, insertion-ordered display,
order-insensitive equality, `union`/`intersect`/`subtract`, `toSet`/`distinct`)
and a wider `Iterable` surface — `sorted`/`sortedDescending`/`take`/`drop` plus
the `associate`/`associateBy`/`minByOrNull`/`none`/`filterNot`/`flatMap`/
`mapIndexed`/`sortedByDescending` higher-order members.

Also landed: **lazy sequences** — `generateSequence(seed) { next }`, ended by a
step that answers `null`, and the one sequence here that is not materialized up
front because it is the one that can be endless. `map`/`filter`/`filterNot`/
`takeWhile`/`dropWhile`/`take`/`drop` append a pipeline stage instead of
running; `first { }`/`find { }`/`any { }` pull one element at a time and stop at
the first match; every other member materializes first, which on an unbounded
pipeline raises rather than hanging (the reference toolchain hangs). The stage
list is copied into each derived sequence, so a base bound to a name is
re-runnable and `dropWhile`'s progress never leaks between pipelines.
`splitToSequence` is `split` — finite either way, so it materializes.

Also landed: `Comparator`s — `compareBy` / `compareByDescending` over one or
more key selectors, extended by `thenBy` / `thenByDescending`, and consumed by
`sortedWith` alongside the plain two-argument lambda it already took. The keys
are kept as a chain rather than folded into one closure, because `thenBy`
answers a NEW comparator (Kotlin's are immutable) and each key carries its own
direction, which a single sign cannot express.

Also landed: the from-the-end and pairing members — `takeLast`/`dropLast` on a
sequence and on a `String`, `unzip`, `zipWithNext` in both its pair-yielding and
its lambda form, and `foldRight`/`reduceRight`, whose lambdas take `(element,
acc)` where `fold`/`reduce` take `(acc, element)`. On a `Map`: `toList` (which
yields `Pair`s, printing `(1, a)` rather than an entry's `1=a`),
`filterKeys`/`filterValues` — each handed that half alone, not the entry — and
`toSortedMap`, recorded as a `TreeMap` so it stays in key order across later
writes.

Also landed: a **real runtime `Char`** — its own value representation rather than
an `Int` code unit, so it prints as a character inside a `List`/`Set`/`Map`,
answers `is Char`, and keeps Kotlin's `Char` arithmetic and ordering inside a
lambda; plus `CharRange` (`'a'..'e'`, `step`/`downTo`/`reversed`/`in`) and the
`Char` classification and case members. The strict-numeric switch it needed also
fixed `+` with a `String` operand in an untyped position (`xs.fold("") { a, b ->
a + b }` concatenates instead of summing). A `data class` may now inherit stored
properties, with its derived members taken from the primary constructor alone.
`super<T>.m()` is parsed and resolved, so a class implementing two supertypes
that both supply `m` can say which one it means. And the inheritance modifiers
are now enforced rather than merely recorded.

Also landed, all pinned by the differential harness against the reference
toolchain: `do { … } while (cond)`; the `when (val n = …)` subject binding and
the erased generic `is List<*>`; hexadecimal/binary integer literals and the
primitive companion constants; **named arguments** for functions, constructors
and `copy`; the bitwise infix members (`and`/`shl`/`inv`/…); `String.format`
with the JVM's HALF_UP `%f` rounding; a `Map` as an iterable (its higher-order
functions, `entries`, `for ((k, v) in m)`, and `filter` re-wrapping into a
`Map`); `break`/`continue` out of a `try` that owns a `finally`; and the local
`return@label`. Two correctness fixes came with them: a non-ASCII string literal
was being lexed byte-per-character (`"café".length` read 5), and a safe call
returning a `String` displayed a null result as the empty string rather than
`null`.

Also landed, each closing a construct that previously failed loudly, and each
pinned by a new differential-harness mode and frozen corpus records captured
from the reference toolchain: **class body properties** (with a `data class`'s
derived members still taken from the primary constructor alone), **extension
functions** dispatched by the receiver's static type, **`companion object`**,
the `this`-receiver **scope functions** (`run`/`apply`/`with`) beside the
existing `it`-form, **default / `vararg` parameters** for every callee kind,
**local `fun`s** that can recurse, **`Pair`/`Triple`**, **captured-`var`
mutation** from a lambda, **`as`/`as?` casts**, **top-level properties** and
**`by lazy`**, and **`runCatching`/`Result`**. Generic declarations and the
`inline`-family modifiers are now accepted and erased, with a reified type test
rejected rather than answered.

The harness caught one silent bug in the process: `Pair(a, b) == Pair(c, d)` was
comparing two heap handles numerically — answering `true` for any two pairs —
because the constructor spelling was not inferred as a heap object the way
`a to b` was.

Also landed, each with a new differential-harness mode (`ctor`, `deleg`,
`invoke`, `strcoll`) and frozen corpus records captured from the reference
toolchain: **secondary constructors** — including the ORDER Kotlin specifies,
where the property initializers and the `init` blocks run interleaved in
declaration order and a secondary's body runs only after the constructor it
delegates to has finished; **interface delegation** (`class C(x: I) : I by x`),
which forwards `I`'s defaulted members too, so a default method calls the
DELEGATE's implementation rather than the delegating class's override, as
Kotlin specifies; **property delegation** through a custom `operator fun
getValue` / `setValue`, which receive the `thisRef` and a `KProperty` carrying
the property's name; **invoking the result of a call** (`f()()`, `lst[0](7)`,
`{ x: Int -> x }(9)`, `f.invoke(x)`, and a class declaring `operator fun
invoke`); and **the collection functions on a `String` receiver**, where the
result TYPE follows `kotlin.text` rather than `Iterable` — `"abc".map { … }` is
a `List<Char>` but `"abc".filter { … }` is a `String`, and `chunked`/`windowed`
answer a `List<String>`.

Three bugs surfaced along the way. Constructor selection needed the ARGUMENT
TYPES, not just the arity: with `class D(val a: Int, val b: Int = 5)` and
`constructor(s: String)`, `D("xyz")` matches both by arity. A class that writes
no primary constructor must prefer a no-argument SECONDARY over the implicit
primary. And the `ctor` harness mode found on its first run that an `init` body
rejected a `;` separator, because it was parsing statements directly instead of
through the shared block rule.

Also landed, with a new `equality` harness mode: **instance equality**, in all
three of the forms described above. A plain class now compares by reference
identity rather than structurally, and a declared `equals`/`hashCode` is
reached from every equality-based member instead of being ignored. Two
supporting changes made that possible. Container dispatch, `Set`/`Map`
construction, indexing and `in` moved from `Op::Extended` to the builtin table,
because fusevm's extension dispatch *takes* the handler out of the VM for the
duration of a call — so an override body running through a nested `vm.run()`
found no handler and every extension op it executed silently did nothing,
leaving it reading its own fields as `Undef`. And `Collection.containsAll` was
added, being equality-based and previously absent.

The widened generator immediately caught a bug older than the feature:
`mapOf`'s repeated-key collapse compared key HANDLES rather than Kotlin
equality, so `mapOf(D(1) to 1, D(1) to 2)` kept two entries — wrong for a
`data class`, a `List` key, or any declared `equals`, and right only for
primitives and `String`. Two more were self-inflicted and caught the same way:
`==` was short-circuiting on identity (Kotlin's `Intrinsics.areEqual` does not,
so a counting `equals` reported one call fewer than the reference toolchain),
and `Set`/`Map` equality compared elements with a bare `equals` where the JVM
goes through the hash-gated `containsAll`/`get`.

The `parity-fuzz` harness also stopped scoring a **barren** run as a pass: a
program the reference toolchain could not compile or run produces no output,
which compares equal to our own failure and used to count as agreement. Such
programs are now reported and counted separately, and they fail the run — which
is how the two generator name-collisions above were found rather than absorbed.

Also landed, with a new `operator` harness mode, closing a silent wrong answer
of the worst kind: the **operator conventions**. Kotlin's operators are not instructions bound to primitive
types — `a + b` *means* `a.plus(b)`, resolved against the left operand like any
other member — and lowering one to an arithmetic op coerced the receiver's
object handle to a number, so `listOf(1, 2, 3) - 2` evaluated to `-2.0`: a
collection operation answering with arithmetic. The whole family now resolves
(`+ - * / %`, `< > <= >=` through `compareTo`, `+=`/`-=` splitting `plus` from
the in-place `plusAssign`, `in` through `contains`, `..` through `rangeTo`,
`[]`/`[]=` through `get`/`set`, unary `-`/`!`, and `++`/`--` through
`inc`/`dec`), against a user class statically and against a `List`/`Set`/`Map`
at run time. An operator the stdlib does not define on a collection now fails
loudly rather than answering a coerced number. The generator had reached none
of this — it produced the arithmetic operators only between numbers and
strings, and named `hashMapOf`/`hashSetOf`/`sortedSetOf`/`groupingBy` not at
all — so the harness had stayed clean over a surface it never visited. Two inference gaps surfaced with
it, both a node the emitter typed one way and inference another: a bare
property read inside a method, and `super<T>.m()`.

Also landed: the **JVM collection constructors** — `HashMap`, `HashSet`,
`LinkedHashMap`, `LinkedHashSet`, `TreeSet`, `ArrayList` — and, with them, the
iteration order those classes actually have. A `HashMap` walks its bucket
TABLE, not its insertion sequence, so `hashMapOf("banana" to 1, "apple" to 2,
"cherry" to 3, "zebra" to 4)` prints `{banana=1, zebra=4, apple=2, cherry=3}`;
`hashMapOf`, `hashSetOf` and `sortedSetOf` had all been quietly answering in
insertion order. The table is modelled as Java builds it — a power-of-two
capacity sized from the element count or the default 16, the
`h xor (h ushr 16)` spread, and a stable ordering by bucket index that
reproduces the resize history — and validated against the reference toolchain
across every size from 1 to 24 for both `Int` and `String` keys. `groupingBy
{ }.eachCount()` landed alongside, lazy as Kotlin's `Grouping` is.

Also landed, with frozen corpus records captured from the reference toolchain:
**`enum class`** — constants with constructor arguments and members, per-constant
bodies over an `abstract fun`, `values`/`entries`/`valueOf`, the `name`
`toString`, and the `ordinal` ordering, so `sorted()` restores declaration order
and `compareTo` answers the ordinal DIFFERENCE the JVM's does. Constants are
singletons on the enum's companion, which is what makes their identity equality
fall out of the rule above rather than needing one of its own. Alongside them:
**properties declared without storage** in an `interface` or as `abstract val`,
satisfied by an `override val` constructor property or by a getter; and
**computed properties** (`val x: Int get() = …`), which have no backing field,
run per read, and dispatch virtually — which is how an interface's default
method reads a property each implementor computes.

Three more silent bugs surfaced with them. A **`when` subject that is an object**
compared heap HANDLES with the native op, so the first arm won whatever the
subject was — a `data class` subject included; the arm is an `==` against the
subject and now goes through the same object equality. `Owner.a == Owner.b`
**inferred neither side as an object** and took that same native path, as did a
comparison of two unannotated `object` properties holding instances. And an
**`object` property named through its owner while the object initialized**
(`val all = listOf(O.A, O.B)`, the shape the enum lowering emits) read the global
the singleton is not published to until every initializer has run.

`toString(radix)` and `toInt(radix)` were **dropping the radix argument** —
answering the base-10 reading, or throwing on a string valid in the base asked
for.

Also landed, with frozen corpus records captured from the reference toolchain:
**`StringBuilder`** — the first mutable JVM object in the heap, and the first
whose members split into a half that mutates and a half inherited from
`CharSequence`. Every mutator (`append`/`appendLine`/`insert`/`delete`/
`replace`/`deleteCharAt`/`reverse`/`clear`) answers the RECEIVER, so a chain
keeps building one object; `setLength`/`setCharAt` answer `Unit`, and
`setLength` pads with NUL (`\u0000`) when it grows. The content is held as UTF-16 code
units rather than a Rust `String`, because every index a builder takes is a JVM
`char` offset and the two disagree the moment a supplementary character appears
— `StringBuilder("a😀b")` has `length` 4, `[1]` is the high surrogate, and
`deleteCharAt(1)` leaves half a pair, none of which a `String` can represent.
`reverse` keeps each surrogate pair facing forward the way
`AbstractStringBuilder` does. `capacity()` is modelled too, growth policy
included (16 by default, `text.length + 16` from a text,
`max(2 * cap + 2, needed)` on an append that does not fit). The read-only half
is delegated to the `String` members rather than written twice. Alongside it:
**`buildString`/`buildList`**, which desugar to `apply` over a fresh builder so
the block's unqualified `append`/`add` is the receiver-scope machinery that
already existed; **`listOfNotNull`** and the `filterNotNull` member; the bulk
mutators **`addAll`/`removeAll`/`retainAll`**, each answering whether the
receiver CHANGED; and the top-level **`repeat`** and the preconditions
**`require`/`requireNotNull`/`check`/`checkNotNull`/`error`/`TODO`**, whose
message block runs only on the failing path and whose `NotImplementedError`
descends from `Error`, so `catch (e: Exception)` does not catch it.

Two bugs surfaced with them. A generic call whose only argument is a trailing
lambda (`buildList<Int> { }`) was **rolling its type-argument list back** — the
scan required a `(` immediately after the `>` — so the whole call re-parsed as a
chain of comparisons and failed on the name. And `StringBuilder(…)` inferred as
an untyped value made `==` **coerce two heap handles** and answer `true` for any
two builders, the same class of bug the `Pair` constructor had; a
`StringBuilder` overrides neither `equals` nor `hashCode`, so identity is the
whole contract.

The hand-assigned host dispatch ids are now **guarded by a test that reads them
back out of the source**. Nothing had been checking them: `register_builtin`
overwrites by id, so two ops that both reach for the next free number merge
without a conflict and the later handler silently replaces the earlier one. The
guard fails on a duplicate id, on an id with no dispatch home or two of them,
and on an emit site that routes an id through the wrong table.

Next: `sequence { … }`/`yield`, written-down type arguments,
`Delegates.observable`/`vetoable`, and a growing standard-library surface —
alongside the sibling parity tooling (LSP/DAP, reference generator, differential
harness).

## [0xFF] LICENSE

MIT. See [LICENSE](LICENSE).
