#!/bin/zsh
# Capture frozen parity records from the REAL Kotlin toolchain.
#
# Reads one program per line from the file named by $1, in the same
# backslash-n encoding `tests/data/parity_expected.txt` uses, compiles and runs
# each through `kotlinc` + `kotlin`, and writes `program<TAB>output` records to
# stdout in the same encoding.
#
# This script NEVER invokes the kotlinrs binary: the corpus is a record of what
# the reference toolchain does, so anything kotlinrs produced would make it a
# record of our own output instead. Append its stdout to the corpus; never
# rewrite existing lines.
#
# DO NOT capture a program whose output names a CLASS LOADER. `kotlin -classpath
# out TKt` — the run step below — loads the program through a URLClassLoader of
# its own, so a `ClassCastException` over a USER class reads `... is in unnamed
# module of loader java.net.URLClassLoader @255316f2`, where `java -cp out TKt`
# and `java -jar` (how a compiled Kotlin program actually runs) both read
# `loader 'app'`. The launcher's spelling also embeds an IDENTITY HASH. Such a
# record freezes the launcher rather than the language; pin it in tests/lang.rs
# against a `java`-run reference instead.
#
#   scripts/capture-parity.sh new-programs.txt >> tests/data/parity_expected.txt
#
# Override the toolchain with KOTLINC / KOTLIN_ORACLE, and the JVM under it
# with CAPTURE_JAVA_HOME.
#
# THE JVM IS PART OF THE ORACLE. `kotlinc` is a launcher script that runs on
# whatever `$JAVA_HOME` names, and a Kotlin program's observable output is not
# the compiler's alone — several answers come straight from the JDK the program
# runs on, and from the JDK the COMPILER ran on for anything it constant-folds:
#
#   * `Double.toString` switched to the shortest-representation algorithm in
#     JDK 19, so `6.0372357323402578E18` on JDK 17 is `6.037235732340258E18`
#     from JDK 19 on. A `const val` folded into a string literal freezes the
#     COMPILER's JVM answer into the class file.
#   * `String`/`StringBuilder` index faults moved onto `Preconditions` in
#     JDK 21: `index 9, length 3` became `Index 9 out of bounds for length 3`.
#
# So an unpinned capture records whichever JDK the capturing shell happened to
# export, and two rounds captured months apart can disagree inside one corpus.
# That is not hypothetical here: record 499 was captured under a shell exporting
# a JDK 17 and froze that JDK's index wording, which no JDK from 21 on produces.
#
# The floor is JDK 21 — the newest of the two cutoffs above — and the script
# refuses to run below it rather than silently recording an older dialect.
#
# THERE ARE TWO JVMs, NOT ONE, and a single program can show both at once. A
# `const val` is folded by the COMPILER, so its rendering is frozen into the
# class file under the compiler's `Double.toString`, while an identical literal
# read at run time renders under the RUNTIME's. Compiling the same file under
# 17 and 21 and running each build under 17 and 21 gives four distinct
# (folded, runtime) pairs:
#
#   compiled=17 run=17   folded=6.0372357323402578E18  runtime=6.0372357323402578E18
#   compiled=17 run=21   folded=6.0372357323402578E18  runtime=6.037235732340258E18
#   compiled=21 run=17   folded=6.037235732340258E18   runtime=6.0372357323402578E18
#   compiled=21 run=21   folded=6.037235732340258E18   runtime=6.037235732340258E18
#
# Exporting `JAVA_HOME` is *expected* to steer both, because both `kotlinc` and
# `kotlin` are launcher scripts that read it — but that is an inference about
# two shell scripts, not a measurement, and it is the inference that would fail
# silently if either launcher ever resolved its JVM another way. So the floor is
# checked twice: once against the `java` `JAVA_HOME` names (the run step) and
# once against the JRE `kotlinc -version` reports for ITSELF (the compile step).
emulate -L zsh
set -uo pipefail

kotlinc=${KOTLINC:-/opt/homebrew/bin/kotlinc}
kotlin=${KOTLIN_ORACLE:-/opt/homebrew/bin/kotlin}
src=${1:?usage: capture-parity.sh PROGRAMS-FILE}

for tool in $kotlinc $kotlin; do
    [[ -x $tool ]] || { print -u2 "capture-parity: $tool is not executable"; exit 2 }
done

# An explicit JVM beats an inherited one; an inherited one beats none.
if [[ -n ${CAPTURE_JAVA_HOME:-} ]]; then
    export JAVA_HOME=$CAPTURE_JAVA_HOME
fi
java=${JAVA_HOME:+$JAVA_HOME/bin/java}
java=${java:-$(command -v java)}
[[ -x $java ]] || {
    print -u2 "capture-parity: no java (set CAPTURE_JAVA_HOME=/path/to/jdk21+)"
    exit 2
}
# `java -version` writes to stderr and spells the feature release first.
jver=$("$java" -version 2>&1 | command perl -ne 'print $1 and last if /version "(\d+)/')
if [[ -z $jver ]] || (( jver < 21 )); then
    print -u2 "capture-parity: $java is JDK ${jver:-unknown}; the corpus needs 21 or newer"
    print -u2 "capture-parity: set CAPTURE_JAVA_HOME to a JDK 21+ home"
    exit 2
fi
# The COMPILER's JVM, measured rather than inferred. `kotlinc -version` spells
# it in its own banner: `info: kotlinc-jvm 2.4.10 (JRE 21.0.12)`.
kver=$($kotlinc -version 2>&1 | tail -1)
cver=$(print -r -- "$kver" | command perl -ne 'print $1 and last if /\(JRE (\d+)/')
if [[ -z $cver ]] || (( cver < 21 )); then
    print -u2 "capture-parity: kotlinc runs on JRE ${cver:-unknown}; the corpus needs 21 or newer"
    print -u2 "capture-parity: $kver"
    print -u2 "capture-parity: set CAPTURE_JAVA_HOME to a JDK 21+ home"
    exit 2
fi
print -u2 "capture-parity: oracle $kver"

# THE LOCALE IS PART OF THE ORACLE TOO. `String.format`'s `%f`/`%e`/`%,d` take
# their decimal separator and grouping from `Locale.getDefault()`, so the same
# program prints `3.14` on an `en_US` machine and `3,14` on a `de_DE` one, and
# every corpus record that goes through `%f`/`%e`/`%,d` would otherwise have
# frozen whichever the capturing machine happened to have. kotlinrs has no
# locale and always formats the `en_US` way, so that is what the oracle is
# pinned to.
# (`Double.toString`, `uppercase()`/`lowercase()` and `sorted()` are NOT
# locale-sensitive — Kotlin's no-argument case members use `Locale.ROOT`.)
#
# AND SO IS THE CONSOLE CHARSET, which `file.encoding` does NOT pin. Through
# JDK 18 it did; JDK 19 split the console streams onto their own
# `stdout.encoding`/`stderr.encoding`, defaulted from the terminal's locale and
# NOT from `file.encoding`. Measured on JDK 21.0.12: with the locale flags but
# without the two console ones, `LANG=C` leaves `file.encoding=UTF-8` but
# `stdout.encoding=US-ASCII`, and `println("café")` writes the bytes `c a f ?`.
# The corpus carries non-ASCII records (one an astral `😀`), so a capture on a
# `LANG=C` machine would have frozen `?` substitutions that look like real output and
# that no locale can reproduce. Pinning the two console streams as well is what
# actually makes the charset independent of the terminal.
jvmflags=(
    -J-Duser.language=en
    -J-Duser.country=US
    -J-Dfile.encoding=UTF-8
    -J-Dstdout.encoding=UTF-8
    -J-Dstderr.encoding=UTF-8
)

work=$(mktemp -d) || exit 2
trap 'rm -rf -- $work' EXIT

typeset -i n=0 bad=0
while IFS= read -r line; do
    [[ -z ${line// } ]] && continue
    rm -rf -- $work/out
    printf '%s' "$line" | command perl -pe 's/\\n/\n/g' > $work/T.kt
    if ! (cd $work && $kotlinc T.kt -d out) >/dev/null 2>&1; then
        print -u2 "capture-parity: kotlinc rejected: $line"
        (( bad++ ))
        continue
    fi
    # THE OUTPUT GOES TO A FILE, NOT TO `$(...)`. Command substitution strips
    # EVERY trailing newline, and the reconstruction that used to follow it put
    # exactly one back on the assumption that every probe ends in a single
    # `println`. Nothing enforced that assumption, and both ways of breaking it
    # minted an expectation the oracle never produced:
    #
    #   fun main() { print("x") }                    → real `x`,     recorded `x\n`
    #   fun main() { println("a"); println(); println() }
    #                                                → real `a\n\n\n`, recorded `a\n`
    #
    # A frozen line like that is unfalsifiable from the replay side — it looks
    # like every other record — and the only way to make the test pass is to
    # break the frontend to match it. Reading the bytes back off disk carries
    # zero, one or many trailing newlines through exactly as they were written.
    (cd $work && $kotlin $jvmflags -classpath out TKt) > $work/out.txt 2>/dev/null
    rc=$?
    if (( rc != 0 )); then
        print -u2 "capture-parity: program exited $rc: $line"
        (( bad++ ))
        continue
    fi
    printf '%s\t' "$line"
    command perl -0pe 's/\n/\\n/g' < $work/out.txt
    print
    (( n++ ))
done < $src

print -u2 "capture-parity: $n record(s) captured, $bad rejected"
(( bad == 0 ))
