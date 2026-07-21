//! Command-line parsing for the `kotlin` binary.
//!
//! Hand-rolled (no arg-parser dependency) to keep the crate's build durable and
//! its dependency surface minimal. Recognizes a script file or `-e` one-liners
//! plus the `--dump-*` introspection flags; anything after the file name is
//! forwarded to the program as `args`.

/// A parsed introspection request (`--dump-*`), mutually exclusive with running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dump {
    Tokens,
    Ast,
    Bytecode,
}

#[derive(Debug, Default)]
pub struct Cli {
    pub file: Option<String>,
    pub eval: Vec<String>,
    pub argv: Vec<String>,
    pub show_version: bool,
    pub show_help: bool,
    pub dump: Option<Dump>,
    /// `--lsp`: speak the Language Server Protocol on stdio.
    pub lsp: bool,
    /// `--dap`: speak the Debug Adapter Protocol on stdio.
    pub dap: bool,
}

/// Parse `std::env::args()` (minus argv[0]).
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => cli.show_help = true,
            "-v" | "--version" => cli.show_version = true,
            "--dump-tokens" => cli.dump = Some(Dump::Tokens),
            "--dump-ast" => cli.dump = Some(Dump::Ast),
            "--dump-bytecode" | "--disasm" => cli.dump = Some(Dump::Bytecode),
            "--lsp" => cli.lsp = true,
            "--dap" => cli.dap = true,
            "-e" | "--eval" => {
                let expr = it.next().ok_or("-e requires an argument")?;
                cli.eval.push(expr);
            }
            // First non-flag is the script; the rest are the program's args.
            _ if cli.file.is_none() && cli.eval.is_empty() && !a.starts_with('-') => {
                cli.file = Some(a);
                cli.argv.extend(it.by_ref());
            }
            _ if !a.starts_with('-') => cli.argv.push(a),
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok(cli)
}
