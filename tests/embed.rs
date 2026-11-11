//! `kotlinrs::run_program`: captured stdout, fed stdin, errors as `Err`,
//! per-call isolation across threads.

use kotlinrs::run_program;

#[test]
fn captures_print_and_println_from_main() {
    let src = "fun main() {\n    print(\"a\")\n    println(1 + 2)\n}\n";
    assert_eq!(run_program(src, "").unwrap(), "a3\n");
}

#[test]
fn captures_builtin_print_path_in_program_with_try() {
    // A `try` switches print to the host builtin instead of the native op.
    let src = "try { println(\"in\") } catch (e: Exception) {}\nprintln(\"out\")";
    assert_eq!(run_program(src, "").unwrap(), "in\nout\n");
}

#[test]
fn stdin_feeds_readline_and_readln() {
    let src = r#"
fun main() {
    val n = readln().toInt()
    var sum = 0
    repeat(n) { sum += readln().trim().toInt() }
    println(sum)
    println(readLine() == null)
}
"#;
    assert_eq!(run_program(src, "3\n10\n20\r\n12").unwrap(), "42\ntrue\n");
}

#[test]
fn readln_past_eof_is_an_error() {
    let err = run_program("println(readln())", "").unwrap_err();
    assert!(err.contains("ReadAfterEOFException"), "{err}");
}

#[test]
fn errors_and_exit_status() {
    assert!(run_program("fun main() { val x = }", "").is_err());
    let err = run_program("println(\"x\")\nthrow IllegalStateException(\"boom\")", "").unwrap_err();
    assert!(err.contains("boom"), "{err}");
    assert_eq!(run_program("println(\"x\")\nkotlin.system.exitProcess(3)", "").unwrap_err(), "exit status 3");
    let out = run_program("println(\"before\")\nSystem.exit(0)\nprintln(\"after\")", "").unwrap();
    assert_eq!(out, "before\n");
}

#[test]
fn concurrent_calls_keep_their_own_output() {
    let src = "fun main() {\n    val tag = readln()\n    for (i in 1..2000) println(tag)\n}\n";
    let handles: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|tag| std::thread::spawn(move || (tag, run_program(src, tag).unwrap())))
        .collect();
    for h in handles {
        let (tag, out) = h.join().unwrap();
        assert_eq!(out, format!("{tag}\n").repeat(2000));
    }
}
