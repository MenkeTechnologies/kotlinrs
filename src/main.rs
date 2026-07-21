//! The `kotlin` binary entry point.
//!
//! Dispatch: `--dump-tokens`/`--dump-ast`/`--dump-bytecode` print the requested
//! stage and exit; otherwise a script file or one or more `-e` snippets are
//! compiled to fusevm bytecode and run. Errors go to stderr in terse
//! `kotlin: <reason>` form; nothing else is printed.

use kotlinrs::cli::{self, Dump};
use std::process::ExitCode;

const USAGE: &str = "\
kotlin — Kotlin on fusevm (compiled, no JVM)

USAGE:
    kotlin [OPTIONS] <file.kt> [args...]
    kotlin -e '<source>' [-e '<source>' ...]

OPTIONS:
    -e, --eval <src>     Run a snippet (repeatable; wrapped in `fun main` if it
                         has none). Joined with newlines.
    --dump-tokens        Print the lexer token stream and exit.
    --dump-ast           Print the parsed AST and exit.
    --dump-bytecode      Print the lowered fusevm chunk (disassembly) and exit.
    --disasm             Alias for --dump-bytecode.
    --lsp                Speak the Language Server Protocol over stdio.
    --dap                Speak the Debug Adapter Protocol over stdio.
    -v, --version        Print the version and exit.
    -h, --help           Print this help and exit.
";

fn main() -> ExitCode {
    let cli = match cli::parse(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };

    if cli.show_help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if cli.show_version {
        println!("{}", kotlinrs::version_banner());
        return ExitCode::SUCCESS;
    }

    // Protocol servers speak JSON-RPC on stdio and never run a script.
    if cli.lsp {
        return match kotlinrs::lsp::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        };
    }
    if cli.dap {
        return match kotlinrs::dap::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        };
    }

    // Resolve the source: `-e` snippets (joined) or a file.
    let (src, is_snippet) = if !cli.eval.is_empty() {
        (cli.eval.join("\n"), true)
    } else if let Some(file) = &cli.file {
        match std::fs::read_to_string(file) {
            Ok(s) => (s, false),
            Err(e) => return fail(&format!("{file}: {e}")),
        }
    } else {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    };

    let prepared = if is_snippet {
        kotlinrs::prepare_source(&src)
    } else {
        src.clone()
    };

    if let Some(dump) = cli.dump {
        let res = match dump {
            Dump::Tokens => kotlinrs::runtime::dump_tokens(&prepared),
            Dump::Ast => kotlinrs::runtime::dump_ast(&prepared),
            Dump::Bytecode => kotlinrs::runtime::dump_bytecode(&prepared),
        };
        return match res {
            Ok(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e),
        };
    }

    match kotlinrs::runtime::run_source(&prepared) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => fail(&e),
    }
}

/// Print `kotlin: <reason>` to stderr and return exit code 1.
fn fail(reason: &str) -> ExitCode {
    eprintln!("kotlin: {reason}");
    ExitCode::FAILURE
}
