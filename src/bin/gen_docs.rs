//! Offline generator for `docs/reference.html` — the keyword / type / builtin
//! reference page, rendered with the same cyberpunk HUD chrome as
//! `docs/index.html`. Run before publishing GitHub Pages:
//!
//! ```sh
//! cargo run --bin gen-docs
//! ```
//!
//! Source of truth: the LSP corpus in `kotlinrs::lsp` (`corpus()`), the exact
//! `(name, chapter, doc, example)` table the editor completion/hover path
//! renders from. The static page and the language server therefore never drift
//! — a name is documented here only if the runtime actually recognizes it in
//! `lexer.rs` (keywords), `ast.rs` (types), or `compiler.rs` (builtins).

use std::fmt::Write as _;

fn main() {
    let corpus = kotlinrs::lsp::corpus();
    let chapter_count = {
        let mut seen: Vec<&str> = Vec::new();
        for (_, c, _, _) in corpus {
            if !seen.contains(c) {
                seen.push(c);
            }
        }
        seen.len()
    };

    let page = format!(
        "{head}{body}{foot}",
        head = HEAD,
        body = build_body(corpus),
        foot = FOOT,
    )
    // Stamp the current crate version so the page never falls behind Cargo.toml.
    .replace("__KOTLINRS_VERSION__", env!("CARGO_PKG_VERSION"));

    let out = "docs/reference.html";
    if let Err(e) = std::fs::write(out, page) {
        eprintln!("gen-docs: cannot write {out}: {e}");
        std::process::exit(1);
    }
    println!(
        "wrote {out} ({} entries, {} chapters)",
        corpus.len(),
        chapter_count
    );
}

/// A reference-corpus entry: (name, chapter, doc, example).
type CEntry<'a> = (&'a str, &'a str, &'a str, &'a str);

/// Render one `<section>` per chapter (first-seen order), each holding one
/// `<article class="doc-entry">` per name: name heading, one-line description,
/// and a runnable usage example.
fn build_body(corpus: &[CEntry]) -> String {
    let mut chapters: Vec<(&str, Vec<&CEntry>)> = Vec::new();
    for entry in corpus {
        let chapter = entry.1;
        match chapters.iter_mut().find(|(c, _)| *c == chapter) {
            Some((_, entries)) => entries.push(entry),
            None => chapters.push((chapter, vec![entry])),
        }
    }

    let mut out = String::new();
    for (chapter, entries) in &chapters {
        let _ = write!(
            out,
            "\n      <section class=\"tutorial-section\" id=\"ch-{slug}\">\n\
             \x20       <h2>{title}</h2>\n",
            slug = slugify(chapter),
            title = html_escape(chapter),
        );
        for (idx, (name, _chapter, doc, example)) in entries.iter().enumerate() {
            let anchor = format!("doc-{}-{}", slugify(chapter), idx + 1);
            let _ = write!(
                out,
                "        <article class=\"doc-entry\" id=\"{anchor}\">\n\
                 \x20         <h3><a class=\"doc-anchor\" href=\"#{anchor}\">#</a> <code>{name}</code></h3>\n\
                 \x20         <p>{doc}</p>\n\
                 \x20         <pre><code class=\"lang-kotlin\">{example}</code></pre>\n\
                 \x20       </article>\n",
                anchor = anchor,
                name = html_escape(name),
                doc = html_escape(doc),
                example = html_escape(example),
            );
        }
        out.push_str("      </section>\n");
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Lowercase, non-alphanumeric runs collapsed to a single `-`, edges trimmed.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

const HEAD: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="dark light">
  <meta name="description" content="kotlinrs — Language reference. Keywords, types, and builtins recognized by the current kotlinrs build. MIT licensed.">
  <title>kotlinrs &mdash; Language Reference</title>
  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
  <link href="https://fonts.googleapis.com/css2?family=Orbitron:wght@400;600;700;900&family=Share+Tech+Mono&display=swap" rel="stylesheet">
  <link rel="stylesheet" href="hud-static.css">
  <link rel="stylesheet" href="tutorial.css">
  <style>
    .tutorial-main { max-width: 76rem; }
    .docs-build-line { margin:0.35rem 0 0;font-family:'Share Tech Mono',ui-monospace,monospace;font-size:11px;color:var(--text-dim);letter-spacing:0.03em;max-width:52rem;opacity:0.75; }
  </style>
</head>
<body>
  <div class="app tutorial-app" id="docsApp">
    <div class="crt-scanline" id="crtH" aria-hidden="true"></div>
    <div class="crt-scanline-v" id="crtV" aria-hidden="true"></div>

    <header class="tutorial-header">
      <div class="tutorial-header-inner">
        <div>
          <h1 class="tutorial-brand">// KOTLINRS — LANGUAGE REFERENCE</h1>
          <nav class="tutorial-crumbs" aria-label="Breadcrumb">
            <a href="index.html">Docs</a>
            <span class="sep">/</span>
            <span class="current">Language Reference</span>
            <span class="sep">/</span>
            <a href="https://github.com/MenkeTechnologies/kotlinrs" target="_blank" rel="noopener noreferrer">GitHub</a>
          </nav>
          <p class="docs-build-line">kotlinrs v__KOTLINRS_VERSION__ · Kotlin on fusevm · lex/parse → AST → bytecode → Cranelift JIT · no JVM · MIT · in active development</p>
        </div>
        <div class="tutorial-toolbar">
          <button type="button" class="btn btn-secondary" id="btnTheme" title="Toggle light/dark">Theme</button>
          <button type="button" class="btn btn-secondary active" id="btnCrt" title="CRT scanline overlay">CRT</button>
          <button type="button" class="btn btn-secondary active" id="btnNeon" title="Neon border pulse">Neon</button>
          <a class="btn btn-secondary" href="index.html">Docs</a>
          <a class="btn btn-secondary" href="https://github.com/MenkeTechnologies/kotlinrs" target="_blank" rel="noopener noreferrer">GitHub</a>
        </div>
      </div>
    </header>

    <main class="tutorial-main">
      <h2 class="tutorial-title"><span class="step-hash">&gt;_</span>LANGUAGE REFERENCE</h2>
      <p class="tutorial-subtitle">Every reserved keyword, built-in type, and builtin call the current kotlinrs build recognizes, grouped by keyword then type then builtin. This page is generated from the language-server corpus (<code>src/lsp.rs</code>) by the <code>gen-docs</code> binary, so it stays in sync with what the runtime and editor tooling actually know about. Keywords mirror <code>lexer.rs</code>, types mirror <code>ast::Type::from_name</code>, and each builtin mirrors a real arm in <code>compiler::compile_call</code>.</p>
"#;

const FOOT: &str = r#"
      <section class="tutorial-section">
        <h2>More</h2>
        <ul>
          <li><strong>Docs</strong> — <a href="index.html">index.html</a> (overview, architecture, examples)</li>
          <li><strong>Engineering report</strong> — <a href="report.html">report.html</a> (value model, status, dependencies)</li>
          <li><strong>Source</strong> — <a href="https://github.com/MenkeTechnologies/kotlinrs">github.com/MenkeTechnologies/kotlinrs</a></li>
        </ul>
      </section>
    </main>

  </div>

  <script src="hud-theme.js"></script>
</body>
</html>
"#;
