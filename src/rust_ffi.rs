//! Kotlin wiring for inline Rust FFI (`rust { ... }` blocks).
//!
//! The heavy lifting lives in fusevm: [`fusevm::RustSugar`] scans and rewrites
//! the block at the source level, and `fusevm::ffi` compiles / loads / marshals
//! it. This module only supplies the Kotlin-flavored [`fusevm::RustSugar`] config
//! and the desugar entry the runtime calls before lexing. The emitted
//! `__rust_compile(...)` call and every exported bareword are resolved in
//! [`crate::compiler`] (the `KT_FFI_COMPILE` / `KT_FFI_CALL` extension ops) and
//! executed by [`crate::host`].
//!
//! A `rust { ... }` block must appear inside a function body — kotlinrs only
//! accepts `fun` declarations at the top level — where the desugar replaces it in
//! place with a `__rust_compile("<base64>", <line>)` call statement.

use fusevm::RustSugar;

/// Emit the Kotlin statement a `rust { ... }` block desugars to: a call to the
/// `__rust_compile` builtin carrying the base64-encoded block body and its line.
/// base64's alphabet (`A-Za-z0-9+/=`) has no `$`, so it needs no escaping inside
/// the double-quoted Kotlin string literal (no interpolation is triggered).
fn emit(b64: &str, line: usize) -> String {
    format!("__rust_compile(\"{b64}\", {line})")
}

/// Kotlin desugar config. Kotlin line comments are `//`, block comments `/* */`.
/// `newline_boundary` is `true` so a `rust { ... }` block that starts a statement
/// line (Kotlin statements need no trailing `;`) is recognized; `{`/`}`/`;` are
/// boundaries too, so a block after an opening brace also matches.
pub const SUGAR: RustSugar = RustSugar {
    keyword: "rust",
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    newline_boundary: true,
    emit,
};

/// Rewrite every `rust { ... }` block in Kotlin source into a `__rust_compile(...)`
/// call, before lexing. No-op when the source has no `rust` token.
pub fn desugar(src: &str) -> String {
    SUGAR.desugar(src)
}

#[cfg(test)]
mod tests {
    #[test]
    fn desugars_block_inside_fun() {
        let src = "fun main() {\n    rust { pub extern \"C\" fn add(a: i64, b: i64) -> i64 { a + b } }\n    println(add(2, 3))\n}\n";
        let out = super::desugar(src);
        assert!(out.contains("__rust_compile("), "no builtin call: {out}");
        assert!(!out.contains("pub extern"), "Rust body leaked: {out}");
        assert!(
            out.contains("println(add(2, 3))"),
            "trailing code lost: {out}"
        );
    }

    #[test]
    fn leaves_ordinary_kotlin_untouched() {
        let src = "fun main() {\n    println(41 + 1)\n}\n";
        assert_eq!(super::desugar(src), src);
    }
}
