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
