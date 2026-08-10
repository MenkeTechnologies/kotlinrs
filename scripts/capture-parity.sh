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
print -u2 "capture-parity: oracle $($kotlinc -version 2>&1 | tail -1)"

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
    out=$( (cd $work && $kotlin -classpath out TKt) 2>/dev/null )
    rc=$?
    if (( rc != 0 )); then
        print -u2 "capture-parity: program exited $rc: $line"
        (( bad++ ))
        continue
    fi
    # `$(...)` strips trailing newlines; every probe here ends in `println`, so
    # put exactly one back and encode it the way the corpus does.
    printf '%s\t%s\\n\n' "$line" "${out//$'\n'/\\n}"
    (( n++ ))
done < $src

print -u2 "capture-parity: $n record(s) captured, $bad rejected"
(( bad == 0 ))
