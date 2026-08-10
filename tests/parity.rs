//! Frozen differential-parity replay.
//!
//! `tests/data/parity_expected.txt` holds a curated corpus of Kotlin programs
//! and the stdout each produces, captured byte-for-byte from the real
//! `kotlinc` + `kotlin` toolchain during authoring (the same programs the
//! `parity-fuzz` binary diffs live). This test replays every program through the
//! built `kotlin` frontend and asserts the frozen output, so the parity-critical
//! behaviours — `Int`-vs-`Double` division, IEEE division by zero,
//! `Double.toString` notation, `String` members, `if` as an expression, list
//! printing — stay locked WITHOUT a Kotlin toolchain installed. CI runs this;
//! the live `parity-fuzz` differential harness is a developer tool.
//!
//! Format: one record per line, `program<TAB>expected`, with newlines in BOTH
//! fields encoded as the two characters backslash-n (Kotlin programs span
//! several lines, unlike the single-line Java records).
//!
//! WHAT THIS REPLAY STRUCTURALLY CANNOT REPORT, since a green run otherwise
//! reads as "no divergence" rather than "none in what is compared":
//!
//!   * **A construct nobody captured.** The corpus is curated, so coverage is
//!     whatever past rounds thought to add. It cannot report a gap in a feature
//!     it holds no record of.
//!   * **A fabricated expectation.** A frozen string is only as good as the
//!     capture that minted it, and this side never re-runs the oracle. A pin
//!     written from memory rather than captured passes here forever — the
//!     JDK-version and locale gates in `scripts/capture-parity.sh` guard the
//!     oracle, not whether a given line ever came from one. Replaying the
//!     corpus through a live JDK 21 toolchain is the only check for that, and
//!     it is a developer step, not a CI one.
//!   * **Anything after the first uncaught throw.** `scripts/capture-parity.sh`
//!     refuses a program that exits non-zero, so no record's expectation can
//!     contain an uncaught exception's output. Every fault in here is caught
//!     and printed by the program itself.
//!   * **Ordering between the two streams.** stdout and stderr are separate
//!     pipes, so how a terminal would interleave them is not compared.
//!
//! stderr's CONTENT and the exit code's VALUE were on that list too. They are
//! not any more: the capture script only mints records from clean runs, so
//! "silent on stderr, exit 0" is a property every record's program has, and
//! [`frozen_corpus_matches_reference_kotlin`] now asserts it.

use std::process::Command;

/// Scratch directories are per-PROCESS, not per-program.
///
/// Keying only on the program's hash gives two concurrent runs of this test the
/// SAME directory for the same record — and the `remove_dir_all` below then
/// deletes a peer's `T.kt` between its write and its read. Several agents share
/// this checkout, so that is a live collision, and it corrupts the run that
/// loses the race rather than failing it. The pid (plus a per-call counter, so
/// one process never reuses a directory either) makes every run disjoint.
static SCRATCH_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One run's whole observable surface, not just the field the corpus froze.
struct Run {
    stdout: String,
    /// The exit status's VALUE. `success()` alone is one bit, and it scores an
    /// oracle exiting 1 and a frontend exiting 134 as the same answer.
    code: Option<i32>,
    stderr: String,
}

fn run(src: &str) -> Run {
    let n = SCRATCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "kotlinrs_parity_{}_{}_{n}",
        std::process::id(),
        fnv1a(src)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("T.kt");
    std::fs::write(&path, src).expect("write program");
    let out = Command::new(env!("CARGO_BIN_EXE_kotlin"))
        .arg(&path)
        .current_dir(&dir)
        .output()
        .expect("spawn kotlin");
    let _ = std::fs::remove_dir_all(&dir);
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        code: out.status.code(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// The corpus is append-only and has never shrunk, so a run that examines
/// fewer records than this is reporting on a TRUNCATED file rather than on the
/// frontend — and `n > 0` would have called that a pass. Raise it with the
/// corpus; never lower it to accommodate a deletion.
const CORPUS_FLOOR: usize = 603;

#[test]
fn frozen_corpus_matches_reference_kotlin() {
    let data = include_str!("data/parity_expected.txt");
    let mut n = 0;
    let mut failures = Vec::new();

    for (i, line) in data.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        // Exactly one TAB per record. A second one would silently move the
        // boundary between the two fields, and `split_once` would report a
        // shorter program against a longer expectation with no complaint.
        assert_eq!(
            line.matches('\t').count(),
            1,
            "line {}: a record holds exactly one TAB separator",
            i + 1
        );
        let (prog_enc, expected_enc) = line
            .split_once('\t')
            .unwrap_or_else(|| panic!("line {}: missing TAB separator", i + 1));
        let prog = prog_enc.replace("\\n", "\n");
        let expected = expected_enc.replace("\\n", "\n");

        let got = run(&prog);
        let show = || prog.replace('\n', " ");
        if got.code != Some(0) {
            failures.push(format!(
                "line {}: frontend exited {:?}, not 0\n  program: {}\n  stderr:  {:?}",
                i + 1,
                got.code,
                show(),
                got.stderr
            ));
        } else if !got.stderr.is_empty() {
            // Every record was minted from a clean reference run — the capture
            // script drops anything that exits non-zero — so the frontend has
            // nothing to say on stderr either. A diagnostic leaking here is a
            // divergence the stdout comparison cannot see.
            failures.push(format!(
                "line {}: frontend wrote to stderr\n  program: {}\n  stderr:  {:?}",
                i + 1,
                show(),
                got.stderr
            ));
        } else if got.stdout != expected {
            failures.push(format!(
                "line {}: output mismatch\n  program:  {}\n  expected: {:?}\n  got:      {:?}",
                i + 1,
                show(),
                expected,
                got.stdout
            ));
        }
        n += 1;
    }

    assert!(
        n >= CORPUS_FLOOR,
        "parity corpus examined {n} record(s), below the {CORPUS_FLOOR} floor — \
         the file is truncated, not the frontend fixed"
    );
    assert!(
        failures.is_empty(),
        "{} of {n} frozen parity case(s) diverged from the reference Kotlin toolchain:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
