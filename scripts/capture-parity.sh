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
#   scripts/capture-parity.sh new-programs.txt >> tests/data/parity_expected.txt
#
# Override the toolchain with KOTLINC / KOTLIN_ORACLE.
emulate -L zsh
set -uo pipefail

kotlinc=${KOTLINC:-/opt/homebrew/bin/kotlinc}
kotlin=${KOTLIN_ORACLE:-/opt/homebrew/bin/kotlin}
src=${1:?usage: capture-parity.sh PROGRAMS-FILE}

for tool in $kotlinc $kotlin; do
    [[ -x $tool ]] || { print -u2 "capture-parity: $tool is not executable"; exit 2 }
done

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
