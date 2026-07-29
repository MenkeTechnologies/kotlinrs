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

use std::process::Command;

fn run(src: &str) -> (String, bool) {
    let dir = std::env::temp_dir().join(format!("kotlinrs_parity_{}", fnv1a(src)));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("T.kt");
    std::fs::write(&path, src).expect("write program");
    let out = Command::new(env!("CARGO_BIN_EXE_kotlin"))
        .arg(&path)
        .current_dir(&dir)
        .output()
        .expect("spawn kotlin");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[test]
fn frozen_corpus_matches_reference_kotlin() {
    let data = include_str!("data/parity_expected.txt");
    let mut n = 0;
    let mut failures = Vec::new();

    for (i, line) in data.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let (prog_enc, expected_enc) = line
            .split_once('\t')
            .unwrap_or_else(|| panic!("line {}: missing TAB separator", i + 1));
        let prog = prog_enc.replace("\\n", "\n");
        let expected = expected_enc.replace("\\n", "\n");

        let (got, ok) = run(&prog);
        if !ok {
            failures.push(format!(
                "line {}: frontend failed\n  program: {}",
                i + 1,
                prog.replace('\n', " ")
            ));
        } else if got != expected {
            failures.push(format!(
                "line {}: output mismatch\n  program:  {}\n  expected: {:?}\n  got:      {:?}",
                i + 1,
                prog.replace('\n', " "),
                expected,
                got
            ));
        }
        n += 1;
    }

    assert!(n > 0, "parity corpus is empty");
    assert!(
        failures.is_empty(),
        "{} of {n} frozen parity case(s) diverged from the reference Kotlin toolchain:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
