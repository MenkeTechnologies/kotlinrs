#!/bin/zsh
# Re-mint every frozen parity record from the LIVE reference toolchain and
# compare it byte for byte against what `tests/data/parity_expected.txt` holds.
#
#   scripts/reverify-parity.sh [CORPUS]
#
# WHY THIS EXISTS. `tests/parity.rs` replays the corpus through the kotlinrs
# frontend and never runs the oracle, so it cannot tell a captured expectation
# from an invented one: a line written from memory rather than measured passes
# there forever, and the only way to make it pass is to break the frontend to
# match it. The JDK-version and locale gates in `capture-parity.sh` guard the
# oracle a capture RAN AGAINST; nothing else checks whether a given line ever
# came from one. This does, and it is the check to run before trusting any pin
# a previous round left behind.
#
# This script NEVER invokes the kotlinrs binary, for the same reason
# `capture-parity.sh` does not: an answer from our own frontend would make the
# comparison a tautology.
#
# TWO STAGES, because the fast one has a known artifact. Compiling 600+ programs
# one at a time costs about ten seconds each, so stage one puts every record in
# its OWN PACKAGE and compiles the lot in a single `kotlinc` — top-level
# declarations cannot then collide. But a package is observable: a user class
# extending a throwable prints its own qualified name, so a record whose output
# contains `NotFound: missing q` re-mints as `p87.NotFound: missing q` and is
# flagged. Stage two re-captures each FLAGGED record alone, in the default
# package, through `capture-parity.sh` itself — the exact path that minted the
# corpus — and a record that agrees there is clean. Only a record that
# disagrees in stage two is a real finding.
#
# THE JVM IS PART OF THE ORACLE and this script says which one it resolved, for
# the compile step and the run step separately, before it compares anything.
# Both must be JDK 21 or newer: `Double.toString` changed in 19 and the
# `String`/`StringBuilder` index faults moved onto `Preconditions` in 21, so an
# older JVM re-mints a different dialect and every disagreement it reports is
# about itself. `kotlinc` is a launcher script that honours an inherited
# `JAVA_HOME`, so an ambient one is overridden here rather than trusted.
emulate -L zsh
setopt extended_glob
set -uo pipefail

here=${0:A:h}
corpus=${1:-$here/../tests/data/parity_expected.txt}
[[ -r $corpus ]] || { print -u2 "reverify-parity: cannot read $corpus"; exit 2 }

export JAVA_HOME=${CAPTURE_JAVA_HOME:-/opt/homebrew/opt/openjdk@21}
kotlinc=${KOTLINC:-/opt/homebrew/bin/kotlinc}
java=$JAVA_HOME/bin/java
stdlib=${KOTLIN_STDLIB:-/opt/homebrew/Cellar/kotlin/2.4.10/libexec/lib/kotlin-stdlib.jar}

[[ -x $kotlinc ]] || { print -u2 "reverify-parity: $kotlinc is not executable"; exit 2 }
[[ -x $java ]] || {
    print -u2 "reverify-parity: no java at $java (set CAPTURE_JAVA_HOME to a JDK 21+ home)"
    exit 2
}
[[ -r $stdlib ]] || {
    print -u2 "reverify-parity: no kotlin-stdlib at $stdlib (set KOTLIN_STDLIB)"
    exit 2
}

# Measured, not inferred — and NAMED, so a wrong answer is attributable.
runjre=$("$java" -version 2>&1 | command perl -ne 'print $1 and last if /version "([0-9.]+)/')
cbanner=$($kotlinc -version 2>&1 | tail -1)
cjre=${${cbanner##*\(JRE }%%\)*}
print -u2 "reverify-parity: JAVA_HOME=$JAVA_HOME"
print -u2 "reverify-parity: compile JVM = JRE ${cjre:-unknown}  ($cbanner)"
print -u2 "reverify-parity: run JVM     = JRE ${runjre:-unknown}  ($java)"
if [[ -z $runjre ]] || (( ${runjre%%.*} < 21 )); then
    print -u2 "reverify-parity: run JVM is ${runjre:-unknown}, below the 21 floor; JAVA_HOME=$JAVA_HOME"
    exit 2
fi
if [[ -z $cjre ]] || (( ${cjre%%.*} < 21 )); then
    print -u2 "reverify-parity: compile JVM is ${cjre:-unknown}, below the 21 floor; JAVA_HOME=$JAVA_HOME"
    exit 2
fi

work=$(mktemp -d) || exit 2
trap 'rm -rf -- $work' EXIT
mkdir -p $work/src

typeset -a prog expect
typeset -i i=0
while IFS= read -r line; do
    [[ -z ${line// } ]] && continue
    (( i++ ))
    prog[$i]=${line%%$'\t'*}
    expect[$i]=${line#*$'\t'}
    { print -r -- "package p$i"
      print -r -- "${prog[$i]}" | command perl -pe 's/\\n/\n/g' } > $work/src/R$i.kt
done < $corpus
(( i )) || { print -u2 "reverify-parity: $corpus holds no records"; exit 2 }
print -u2 "reverify-parity: $i record(s); compiling as one batch"

if ! (cd $work && $kotlinc src -d out) >$work/kc.txt 2>&1; then
    print -u2 "reverify-parity: the batch compile FAILED — the corpus holds a program"
    print -u2 "reverify-parity: this toolchain rejects, which is itself a finding:"
    command perl -ne 'print "  $_" if /error:/' $work/kc.txt | head -20 >&2
    exit 3
fi

# The locale and the two console streams are pinned exactly as the capture
# script pins them — `file.encoding` alone stopped covering stdout in JDK 19.
jvmflags=(
    -Duser.language=en
    -Duser.country=US
    -Dfile.encoding=UTF-8
    -Dstdout.encoding=UTF-8
    -Dstderr.encoding=UTF-8
)

typeset -a flagged
typeset -i n=0
for j in {1..$i}; do
    (cd $work && timeout 30 $java $jvmflags -cp "out:$stdlib" p$j.R${j}Kt) \
        >$work/o.txt 2>$work/e.txt
    rc=$?
    (( n++ ))
    # Read off DISK. A command substitution strips every trailing newline, which
    # is the exact defect that let fabricated pins into this corpus once.
    got=$(command perl -0pe 's/\n/\\n/g' < $work/o.txt)
    if (( rc != 0 )); then
        print -r -- "RECORD $j: the reference exited $rc"
        print -r -- "  program: ${prog[$j]}"
        command perl -ne 'print "  stderr:  $_" and last' $work/e.txt
        flagged+=$j
    elif [[ $got != ${expect[$j]} ]]; then
        print -r -- "RECORD $j: re-minted output differs"
        print -r -- "  program: ${prog[$j]}"
        print -r -- "  frozen:  ${expect[$j]}"
        print -r -- "  live:    $got"
        flagged+=$j
    fi
done

(( ${#flagged} == 0 )) && {
    print -u2 "reverify-parity: $n record(s) re-minted, all agree byte for byte"
    exit 0
}

# STAGE TWO. Re-capture each flagged record ALONE and in the default package,
# through the very script that minted the corpus, so the package prefix stage
# one adds cannot be mistaken for a fabricated pin.
print -u2 "reverify-parity: ${#flagged} record(s) flagged; re-capturing each in the default package"
typeset -i real=0
for j in $flagged; do
    print -r -- "${prog[$j]}" > $work/one.txt
    if ! $here/capture-parity.sh $work/one.txt > $work/one.rec 2>$work/one.err; then
        print -r -- "RECORD $j CONFIRMED: capture-parity.sh could not mint it"
        command perl -ne 'print "  $_"' $work/one.err | head -3
        (( real++ ))
        continue
    fi
    got=${$(command perl -pe 's/^.*?\t//' $work/one.rec)}
    if [[ $got == ${expect[$j]} ]]; then
        print -r -- "record $j: clean — stage one's package prefix, not a bad pin"
    else
        print -r -- "RECORD $j CONFIRMED FABRICATED OR STALE"
        print -r -- "  program: ${prog[$j]}"
        print -r -- "  frozen:  ${expect[$j]}"
        print -r -- "  live:    $got"
        (( real++ ))
    fi
done

print -u2 "reverify-parity: $n re-minted, ${#flagged} flagged, $real confirmed"
(( real == 0 ))
