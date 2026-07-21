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
    let mut vm = VM::new(chunk);
    host::install(&mut vm);
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
    let src = crate::rust_ffi::desugar(src);
    let program = parser::parse_program(&src)?;
    let chunk = compiler::compile(&program)?;
    Ok(chunk.disassemble())
}
