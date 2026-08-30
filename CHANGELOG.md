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
