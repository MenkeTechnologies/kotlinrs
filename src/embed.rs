//! Embedding entry point: run a Kotlin program from Rust with a given stdin
//! and get its stdout back as a string.

use crate::{host, runtime};

/// Run `src` as a complete program with `input` as its stdin and return
/// everything it wrote to stdout.
///
/// `src` may declare `fun main` or be a bare statement snippet (wrapped in
/// `main`, as `kotlin -e` does). Each call runs on its own interpreter thread,
/// so heap, exception and JIT state start fresh and concurrent calls never
/// share an output buffer. `readLine()` / `readln()` read `input` line by
/// line; `print` / `println` append to the returned string and never reach the
/// process stdout. Stderr output (`System.err`) still goes to the process
/// stderr.
///
/// Returns `Err` for a parse or compile error, an uncaught exception, a
/// nonzero `exitProcess` / `System.exit`, or an internal panic. `exitProcess(0)`
/// returns `Ok` with the output written up to that point.
pub fn run_program(src: &str, input: &str) -> Result<String, String> {
    let (src, input) = (crate::prepare_source(src), input.to_string());
    let (src2, input2) = (src.clone(), input.clone());
    runtime::on_interpreter_thread(
        move || run_captured(&src, &input),
        || run_captured(&src2, &input2),
    )
}

/// The whole embedded run on the current thread: capture on, run, capture off.
fn run_captured(src: &str, input: &str) -> Result<String, String> {
    host::begin_embedded_io(input);
    let result = std::panic::catch_unwind(|| runtime::run_source_on_this_thread(src));
    let output = host::end_embedded_io();
    match result {
        Ok(Ok(0)) => Ok(output),
        Ok(Ok(code)) => Err(format!("exit status {code}")),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("internal error: the interpreter panicked".to_string()),
    }
}
