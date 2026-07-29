//! Drive a Kotlin source string through the pipeline: parse → lower to a
//! `fusevm::Chunk` → run on a fresh VM with the Kotlin extension handler
//! installed.

use crate::{compiler, host, parser};
use fusevm::{VMResult, VM};

/// Parse, compile, and run `src`. Returns the process exit code (`0` on normal
/// completion) or an error string for a compile error or uncaught exception.
pub fn run_source(src: &str) -> Result<i32, String> {
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
