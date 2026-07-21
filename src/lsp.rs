//! Language Server Protocol over stdio (`kotlin --lsp`).
//!
//! Self-contained and read-only: diagnostics come from the same
//! `parser::parse_program` the runtime uses (a syntax error maps to the reported
//! line); hover and completion draw on the keyword/type/builtin corpus below. No
//! output ever reaches the terminal — JSON-RPC on stdio only. Structure follows
//! the sibling `-rs` frontends' `lsp.rs` (see `pythonrs/src/lsp.rs`).

use std::collections::HashMap;

use lsp_server::{Connection, ErrorCode, ExtractError, Message, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{Completion, HoverRequest, Request as _};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, Hover, HoverContents, HoverParams, HoverProviderCapability,
    MarkupContent, MarkupKind, Position, PublishDiagnosticsParams, Range, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions, Uri,
};

/// The keyword / type / builtin corpus: (name, chapter, one-line doc, example).
/// Single source of truth for LSP completion and hover, and for the offline
/// `gen-docs` reference page. Every entry mirrors something the runtime actually
/// recognizes:
///   * "Keyword" → a reserved word emitted by `lexer.rs` (`fun`, `val`, `for`, …)
///   * "Type"    → a type name resolved by `ast::Type::from_name`
///   * "Builtin" → a call arm handled in `compiler::compile_call`
const CORPUS: &[(&str, &str, &str, &str)] = &[
    // ── Keyword ──
    (
        "fun",
        "Keyword",
        "declare a function; execution enters `fun main`",
        "fun square(n: Int): Int { return n * n }",
    ),
    (
        "val",
        "Keyword",
        "declare a read-only (immutable) binding",
        "val x = 41\nprintln(x + 1)   // 42",
    ),
    (
        "var",
        "Keyword",
        "declare a reassignable (mutable) binding",
        "var i = 0\ni += 1   // i == 1",
    ),
    (
        "if",
        "Keyword",
        "conditional branch; also usable as an expression",
        "val m = if (a > b) a else b",
    ),
    (
        "else",
        "Keyword",
        "the fallback branch of an `if`",
        "if (n % 2 == 0) println(\"even\") else println(\"odd\")",
    ),
    (
        "while",
        "Keyword",
        "loop while the condition stays true",
        "var i = 0\nwhile (i < 3) { i += 1 }",
    ),
    (
        "for",
        "Keyword",
        "iterate a range: `for (x in a..b)`",
        "for (i in 1..5) println(i)",
    ),
    (
        "in",
        "Keyword",
        "the `for (x in range)` separator",
        "for (c in 0 until 3) println(c)",
    ),
    (
        "return",
        "Keyword",
        "return a value (or Unit) from the current function",
        "fun answer(): Int { return 42 }",
    ),
    (
        "until",
        "Keyword",
        "half-open ascending range: `a until b` excludes b",
        "for (i in 0 until 3) println(i)   // 0 1 2",
    ),
    (
        "downTo",
        "Keyword",
        "descending inclusive range: `a downTo b`",
        "for (i in 3 downTo 1) println(i)   // 3 2 1",
    ),
    (
        "step",
        "Keyword",
        "stride for a range: `a..b step n`",
        "for (i in 0..10 step 2) println(i)",
    ),
    (
        "true",
        "Keyword",
        "the Boolean true literal",
        "val ok: Boolean = true",
    ),
    (
        "false",
        "Keyword",
        "the Boolean false literal",
        "val done: Boolean = false",
    ),
    (
        "when",
        "Keyword",
        "multi-way branch (statement or expression); subject or subjectless",
        "when (n) { 1 -> \"one\"; in 2..9 -> \"few\"; else -> \"many\" }",
    ),
    (
        "is",
        "Keyword",
        "runtime type check in a `when` arm: `is String`, `!is Int`",
        "when (x) { is String -> \"str\"; is Int -> \"int\"; else -> \"?\" }",
    ),
    (
        "break",
        "Keyword",
        "exit the enclosing loop; `break@label` targets a labeled loop",
        "for (i in 1..9) { if (i == 5) break }",
    ),
    (
        "continue",
        "Keyword",
        "skip to the loop's next iteration; `continue@label` for a labeled loop",
        "for (i in 1..9) { if (i % 2 == 0) continue; println(i) }",
    ),
    (
        "null",
        "Keyword",
        "the null reference; used with `T?`, `?.`, `?:`, and `!!`",
        "val x: Int? = null\nprintln(x ?: 0)   // 0",
    ),
    // ── Type ──
    (
        "Int",
        "Type",
        "32/64-bit signed integer; `/` and `%` truncate toward zero",
        "val n: Int = 7 / 2   // 3",
    ),
    (
        "Long",
        "Type",
        "64-bit signed integer; shares Int's integer division rules",
        "val big: Long = 1000",
    ),
    (
        "Double",
        "Type",
        "IEEE-754 double; prints with a trailing `.0` when whole",
        "val d: Double = 3.0   // \"3.0\"",
    ),
    (
        "Float",
        "Type",
        "floating-point value (coerced to Double behavior here)",
        "val f: Float = 1.5f",
    ),
    (
        "Boolean",
        "Type",
        "true/false; prints as `true` or `false`",
        "val b: Boolean = 1 < 2",
    ),
    (
        "Char",
        "Type",
        "a single character (code unit); integral — `'A' + 1`, `.code`, `.toChar()`",
        "val c: Char = 'A'\nprintln(c + 1)   // B",
    ),
    (
        "String",
        "Type",
        "text; `+` concatenates and `\"$x\"` interpolates",
        "val s: String = \"n = ${1 + 1}\"",
    ),
    (
        "Unit",
        "Type",
        "the no-value type; the result of a function with no return",
        "fun log(): Unit { println(\"hi\") }",
    ),
    // ── Builtin ──
    (
        "println",
        "Builtin",
        "write a value to stdout followed by a newline",
        "println(6 * 7)   // 42",
    ),
    (
        "print",
        "Builtin",
        "write a value to stdout with no trailing newline",
        "print(\"a\"); print(\"b\")   // ab",
    ),
];

/// The reference corpus, exposed for offline doc generation (`gen-docs`).
pub fn corpus() -> &'static [(&'static str, &'static str, &'static str, &'static str)] {
    CORPUS
}

/// Open document text keyed by URI, kept current from the sync notifications so
/// hover can look up the identifier under the cursor.
type Docs = HashMap<String, String>;

/// Entry point for `kotlin --lsp`.
pub fn run() -> Result<(), String> {
    spawn_orphan_guard();
    let (conn, io_threads) = Connection::stdio();
    let (init_id, _params) = conn
        .initialize_start()
        .map_err(|e| format!("lsp initialize: {e}"))?;
    let init_result = serde_json::json!({
        "capabilities": server_capabilities(),
        "serverInfo": { "name": "kotlinrs", "version": env!("CARGO_PKG_VERSION") },
    });
    conn.sender
        .send(Response::new_ok(init_id, init_result).into())
        .map_err(|e| format!("lsp send: {e}"))?;

    let mut docs: Docs = HashMap::new();
    for msg in &conn.receiver {
        match msg {
            Message::Request(req) => {
                if conn
                    .handle_shutdown(&req)
                    .map_err(|e| format!("lsp shutdown: {e}"))?
                {
                    break;
                }
                dispatch_request(&conn, &docs, req);
            }
            Message::Notification(not) => dispatch_notification(&conn, &mut docs, not),
            Message::Response(_) => {}
        }
    }
    drop(conn);
    io_threads.join().map_err(|_| "lsp io join".to_string())?;
    Ok(())
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                ..Default::default()
            },
        )),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(false),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        ..Default::default()
    }
}

fn handle<P, R>(conn: &Connection, req: Request, f: impl FnOnce(P) -> R)
where
    P: serde::de::DeserializeOwned,
    R: serde::Serialize,
{
    let method = req.method.clone();
    let id = req.id.clone();
    match req.extract::<P>(&method) {
        Ok((id, params)) => {
            let value = serde_json::to_value(f(params)).unwrap_or(serde_json::Value::Null);
            let _ = conn.sender.send(Response::new_ok(id, value).into());
        }
        Err(ExtractError::JsonError { error, .. }) => {
            let _ = conn.sender.send(
                Response::new_err(id, ErrorCode::InvalidParams as i32, error.to_string()).into(),
            );
        }
        Err(ExtractError::MethodMismatch(_)) => unreachable!("method matched before extract"),
    }
}

fn dispatch_request(conn: &Connection, docs: &Docs, req: Request) {
    match req.method.as_str() {
        Completion::METHOD => handle(conn, req, |_p: CompletionParams| completions()),
        HoverRequest::METHOD => handle(conn, req, |p: HoverParams| hover(docs, &p)),
        _ => {
            let _ = conn.sender.send(
                Response::new_err(req.id, ErrorCode::MethodNotFound as i32, "unhandled".into())
                    .into(),
            );
        }
    }
}

fn dispatch_notification(conn: &Connection, docs: &mut Docs, not: lsp_server::Notification) {
    match not.method.as_str() {
        DidOpenTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(not.params) {
                let uri = p.text_document.uri;
                docs.insert(uri.as_str().to_string(), p.text_document.text.clone());
                publish_diagnostics(conn, &uri, &p.text_document.text);
            }
        }
        DidChangeTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(not.params) {
                if let Some(change) = p.content_changes.into_iter().last() {
                    let uri = p.text_document.uri;
                    docs.insert(uri.as_str().to_string(), change.text.clone());
                    publish_diagnostics(conn, &uri, &change.text);
                }
            }
        }
        DidCloseTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(not.params) {
                let uri = p.text_document.uri;
                docs.remove(uri.as_str());
                publish_diagnostics(conn, &uri, "");
            }
        }
        _ => {}
    }
}

fn completions() -> CompletionResponse {
    let items = CORPUS
        .iter()
        .map(|(name, chapter, doc, _example)| CompletionItem {
            label: name.to_string(),
            kind: Some(match *chapter {
                "Keyword" => CompletionItemKind::KEYWORD,
                "Type" => CompletionItemKind::CLASS,
                _ => CompletionItemKind::FUNCTION,
            }),
            detail: Some((*doc).to_string()),
            ..Default::default()
        })
        .collect();
    CompletionResponse::Array(items)
}

/// Hover: look up the identifier under the cursor in the corpus and render its
/// chapter, doc, and example. Falls back to a short banner otherwise.
fn hover(docs: &Docs, params: &HoverParams) -> Hover {
    let pos = params.text_document_position_params.position;
    let uri = params
        .text_document_position_params
        .text_document
        .uri
        .as_str();
    let word = docs
        .get(uri)
        .and_then(|text| word_at(text, pos))
        .unwrap_or_default();

    let matches: Vec<&(&str, &str, &str, &str)> =
        CORPUS.iter().filter(|(name, ..)| *name == word).collect();

    let body = if matches.is_empty() {
        "**kotlinrs** — Kotlin on the fusevm bytecode VM + Cranelift JIT.".to_string()
    } else {
        let mut out = String::new();
        for (name, chapter, doc, example) in matches {
            out.push_str(&format!(
                "**`{name}`** — _{chapter}_\n\n{doc}\n\n```kotlin\n{example}\n```\n\n"
            ));
        }
        out.trim_end().to_string()
    };

    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: body,
        }),
        range: None,
    }
}

/// Extract the identifier (`[A-Za-z0-9_]+`) spanning the given position, if any.
fn word_at(text: &str, pos: Position) -> Option<String> {
    let line = text.lines().nth(pos.line as usize)?;
    let chars: Vec<char> = line.chars().collect();
    let col = (pos.character as usize).min(chars.len());
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';

    let mut start = col;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some(chars[start..end].iter().collect())
}

fn publish_diagnostics(conn: &Connection, uri: &Uri, text: &str) {
    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics: compute_diagnostics(text),
        version: None,
    };
    let not = lsp_server::Notification::new(PublishDiagnostics::METHOD.to_string(), params);
    let _ = conn.sender.send(not.into());
}

/// Parse the whole document with the runtime's own parser; a syntax error maps
/// to a single diagnostic on the line named in its `(line N)` suffix.
fn compute_diagnostics(text: &str) -> Vec<Diagnostic> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    // Snippets without `fun main` are wrapped the same way the runtime wraps a
    // `-e` one-liner, so an editor scratch buffer of bare statements does not
    // report a spurious "expected fun" error.
    let prepared = crate::prepare_source(text);
    let wrapped = prepared != text;
    match crate::parser::parse_program(&prepared) {
        Ok(_) => Vec::new(),
        Err(e) => {
            // When wrapped, the reported line is offset by the injected `fun main`
            // header line; shift it back so it points at the user's source.
            let raw = parse_error_line(&e);
            let line = if wrapped { raw.saturating_sub(1) } else { raw }.saturating_sub(1);
            vec![Diagnostic {
                range: Range {
                    start: Position { line, character: 0 },
                    end: Position {
                        line,
                        character: 200,
                    },
                },
                severity: Some(DiagnosticSeverity::ERROR),
                message: e,
                ..Default::default()
            }]
        }
    }
}

/// Extract the (1-based) line number from a kotlinrs parser error, which embeds
/// it as `… (line N)`. Defaults to line 1 when no such marker is present.
fn parse_error_line(e: &str) -> u32 {
    e.rsplit_once("(line ")
        .and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(1)
}

/// Exit if reparented to pid 1 (the editor died) so we never leak.
fn spawn_orphan_guard() {
    std::thread::spawn(|| {
        #[cfg(target_os = "linux")]
        // SAFETY: prctl(PR_SET_PDEATHSIG, ...) only registers a signal disposition.
        unsafe {
            libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0,
                0,
                0,
            );
        }
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            // SAFETY: getppid takes no arguments and never fails.
            if unsafe { libc::getppid() } == 1 {
                std::process::exit(0);
            }
        }
    });
}
