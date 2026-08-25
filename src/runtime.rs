//! Drive a Kotlin source string through the pipeline: parse → lower to a
//! `fusevm::Chunk` → run on a fresh VM with the Kotlin extension handler
//! installed.

use crate::{compiler, host, parser};
use fusevm::{VMResult, VM};

/// The interpreter thread's stack, in bytes.
///
/// A closure body, an `equals`/`hashCode`/`toString` override and a
/// lambda-taking collection member all run through a re-entrant `vm.run()`
/// (`host::run_sub`), so one level of Kotlin recursion through any of them
/// costs one Rust frame of the whole interpreter dispatch loop. Those frames
/// are large — measured at roughly 100 KB each in a `cargo build` (dev, no
/// optimization) binary, where every local of the dispatch `match` gets its own
/// slot — so the platform default main-thread stack (8 MiB on macOS) runs out
/// at a recursion depth under 100.
///
/// That is both a capability floor and a correctness problem: a Rust stack
/// overflow is `SIGABRT`, which no `catch` can see, where Kotlin raises a
/// perfectly catchable `StackOverflowError`. Running the program on a thread
/// whose stack is reserved up front moves the floor somewhere useful, and
/// [`host::NESTED_RUN_LIMIT`] then raises the JVM's throwable *before* this
/// stack can be exhausted. Both halves are needed: the big stack alone only
/// moves the abort, and the limit alone would reject shallow, legal programs.
///
/// This is a virtual reservation. Only the pages a run actually touches are
/// committed, so a `println("hi")` costs nothing for it.
const INTERPRETER_STACK: usize = 1 << 29; // 512 MiB

/// Parse, compile, and run `src` on the interpreter thread.
///
/// Every piece of run state `host` keeps is `thread_local!` (the pending
/// exception, the `finally` stash, the catchability flag, the parked fault), so
/// the whole pipeline — compile, install, run, and *collect the error* — has to
/// happen on the one thread. It does: this spawns [`run_source_on_this_thread`]
/// and hands back exactly what it returned.
pub fn run_source(src: &str) -> Result<i32, String> {
    let owned = src.to_string();
    on_interpreter_thread(
        move || run_source_on_this_thread(&owned),
        || run_source_on_this_thread(src),
    )
}

/// Run `work` on a freshly spawned interpreter thread — the one with
/// `INTERPRETER_STACK` reserved — and hand back what it returned. `inline` is
/// the fallback for a system that cannot give us a thread at all.
///
/// Anything that has to observe the run's *own* thread state belongs in `work`
/// rather than after the call: `host`'s run state is `thread_local!`, and so is
/// fusevm's compiled-trace cache, so a caller that runs here and then asks
/// about the result from the spawning thread is asking a different thread's
/// tables. That is exactly what [`crate::tiers::report`] used to do — it ran
/// the program here and then read the trace cache on the main thread, which
/// that run never wrote to, so every program it reported on came back
/// `traced=false` whatever the tiers had actually done with it.
pub fn on_interpreter_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
    inline: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let worker = std::thread::Builder::new()
        .name("kotlin".to_string())
        .stack_size(INTERPRETER_STACK)
        .spawn(work);
    match worker {
        Ok(h) => match h.join() {
            Ok(r) => r,
            // The interpreter thread panicked. That is a kotlinrs bug, not a
            // Kotlin exception, so it is reported as one rather than dressed up
            // as a throwable a program could have caught.
            Err(_) => Err("internal error: the interpreter thread panicked".to_string()),
        },
        // No thread available (a hard resource limit). Running inline is worse
        // than not running at all only in stack depth, so fall back rather than
        // refuse the program.
        Err(_) => inline(),
    }
}

/// [`run_source`] without the thread hop — the whole pipeline, inline.
pub fn run_source_on_this_thread(src: &str) -> Result<i32, String> {
    let src = crate::rust_ffi::desugar(src);
    let program = parser::parse_program(&src)?;
    let chunk = compiler::compile(&program)?;
    let _ = host::take_error(); // clear any stale fault from a prior run
                                // A runtime fault that names a JVM throwable is catchable only in a program
                                // that has a `try` — the only program whose bytecode carries the unwind
                                // checks that would deliver it to a handler.
    host::set_catchable(compiler::uses_exceptions(&program));
    let mut vm = VM::new(chunk);
    host::install(&mut vm);
    // A `Char` is not a number to fusevm, so the VM runs under the strict
    // numeric policy: `'a' + 1` and `c < 'z'` — native ops even where the
    // compiler cannot see a type — reach Kotlin instead of being coerced.
    host::install_numeric(&mut vm);
    // Arm the tracing tier: a hot loop whose body is native fusevm ops gets
    // recorded and compiled to native code instead of dispatched forever.
    // `kotlin --tiers` reports whether a given script actually reaches it.
    vm.enable_tracing_jit();
    match vm.run() {
        VMResult::Ok(_) | VMResult::Halted => {
            // An uncaught runtime fault (e.g. integer `/ by zero`) halts the VM
            // and parks its message here.
            if let Some(err) = host::take_error() {
                return Err(err);
            }
            Ok(0)
        }
        VMResult::Error(e) => Err(e),
    }
}

/// `--dump-tokens`: the lexer output, one token per line.
pub fn dump_tokens(src: &str) -> Result<String, String> {
    let src = crate::rust_ffi::desugar(src);
    let toks = crate::lexer::Lexer::new(&src).tokenize()?;
    let mut out = String::new();
    for t in &toks {
        out.push_str(&format!("{:>4}  {:?}\n", t.line, t.tok));
    }
    Ok(out)
}

/// `--dump-ast`: the parsed program as a pretty-printed AST.
pub fn dump_ast(src: &str) -> Result<String, String> {
    let src = crate::rust_ffi::desugar(src);
    let program = parser::parse_program(&src)?;
    Ok(format!("{program:#?}\n"))
}

/// `--dump-bytecode` / `--disasm`: the lowered fusevm chunk, disassembled.
pub fn dump_bytecode(src: &str) -> Result<String, String> {
    Ok(compile(src)?.disassemble())
}

/// Desugar, parse, and lower `src` to the fusevm chunk [`run_source`] executes.
/// Shared by `--dump-bytecode` and by [`crate::tiers`], so the chunk a tier
/// report inspects is the one the run compiled.
pub fn compile(src: &str) -> Result<fusevm::Chunk, String> {
    let src = crate::rust_ffi::desugar(src);
    let program = parser::parse_program(&src)?;
    compiler::compile(&program)
}
