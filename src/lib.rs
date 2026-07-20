//! kotlinrs — Kotlin as a [`fusevm`] frontend.
//!
//! The library lexes and parses a Kotlin subset ([`lexer`], [`parser`]), lowers
//! it to a `fusevm::Chunk` ([`compiler`]), and runs it on the shared bytecode VM
//! with a small Kotlin extension handler ([`host`]). There is no VM or JIT here:
//! arithmetic, comparison, control flow, locals, and calls are *native* fusevm
//! ops, so the engine's three-tier Cranelift JIT can trace hot loops.
//!
//! ```
//! let out = kotlinrs::run("fun main() { println(6 * 7) }");
//! assert!(out.is_ok());
//! ```

pub mod ast;
pub mod cli;
pub mod compiler;
pub mod host;
pub mod lexer;
pub mod parser;
pub mod runtime;
pub mod token;

use lexer::Lexer;
use token::Tok;

/// Crate version, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// One-line version banner (`--version`).
pub fn version_banner() -> String {
    format!("kotlinrs {VERSION} — Kotlin on fusevm (compiled, no JVM)")
}

/// True when the source already declares `fun main` — otherwise a snippet is
/// wrapped as a `main` body (script mode, used for `-e` one-liners).
pub fn has_main(src: &str) -> bool {
    let toks = match Lexer::new(src).tokenize() {
        Ok(t) => t,
        Err(_) => return false,
    };
    toks.windows(2)
        .any(|w| w[0].tok == Tok::Fun && matches!(&w[1].tok, Tok::Ident(n) if n == "main"))
}

/// Wrap a bare statement snippet in `fun main { … }` when it has no `main`, so
/// `-e 'println(1)'` runs like a script.
pub fn prepare_source(src: &str) -> String {
    if has_main(src) {
        src.to_string()
    } else {
        format!("fun main() {{\n{src}\n}}\n")
    }
}

/// Compile and run a full Kotlin source string (must contain `fun main`).
pub fn run(src: &str) -> Result<i32, String> {
    runtime::run_source(src)
}

/// Compile and run a snippet, wrapping it in `main` if needed (`-e` mode).
pub fn run_snippet(src: &str) -> Result<i32, String> {
    runtime::run_source(&prepare_source(src))
}
