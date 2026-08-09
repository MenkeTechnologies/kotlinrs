//! Recursive-descent parser for the Kotlin subset.
//!
//! Grammar (informal):
//! ```text
//! program  := funDecl*
//! funDecl  := 'fun' IDENT '(' params? ')' (':' TYPE)? (block | '=' expr)
//! block    := '{' stmt* '}'
//! stmt     := letDecl | 'return' expr? | 'while' '(' expr ')' block
//!           | 'for' '(' IDENT 'in' range ')' block | ifStmt | assign | exprStmt
//! expr     := or
//! or       := and ('||' and)*
//! and      := eq  ('&&' eq)*
//! eq       := cmp (('=='|'!=') cmp)*
//! cmp      := inOp (('<'|'>'|'<='|'>=') inOp)*
//! inOp     := elvis (('in'|'!in') elvis)*
//! elvis    := infix ('?:' elvis)?
//! infix    := range (('to'|'until'|'downTo'|'step') range)*
//! range    := add ('..' add)*
//! add      := mul (('+'|'-') mul)*
//! mul      := unary (('*'|'/'|'%') unary)*
//! unary    := ('-'|'!'|'++'|'--') unary | postfix
//! postfix  := primary ('.' IDENT ('(' args? ')')?)* ('++'|'--')?
//! primary  := INT | FLOAT | STRING | BOOL | ifExpr | call | IDENT | '(' expr ')'
//! ```
//!
//! The `inOp`/`elvis`/`infix`/`range` levels mirror Kotlin's own precedence
//! table: `..` binds tighter than the named infix functions (`1..10 step 2` is
//! `(1..10) step 2`), which bind tighter than `?:`, which binds tighter than
//! `in`, which binds tighter than the comparisons.

use crate::ast::*;
use crate::lexer::Lexer;
use crate::token::{Spanned, StrPart, Tok};
use std::collections::HashMap;

pub struct Parser {
    toks: Vec<Spanned>,
    pos: usize,
    /// Parameter types collected while scanning function-type annotations, in
    /// source order. [`Parser::type_ref`] appends; the `val`/`var` rule takes
    /// the slice its own annotation contributed. A side channel rather than a
    /// return value because a function type may nest inside a generic argument
    /// several `type_ref` frames down, where the caller has nowhere to put it.
    fn_param_types: Vec<Type>,
    /// The RESULT types of the function types seen since the last checkpoint,
    /// appended by the same [`Parser::type_ref`] frames that fill
    /// `fn_param_types`. A nested function type (`(Int) -> (Int) -> Int`)
    /// finishes parsing before its enclosing one, so the OUTERMOST annotation is
    /// the last entry — which is the one a call through the binding yields.
    fn_ret_types: Vec<Type>,
    /// The type-variable name the most recent [`Parser::type_ref`] resolved, or
    /// `None` if that annotation named a real type. Every exit path of
    /// `type_ref` sets it, so a caller reads it immediately after the call to
    /// learn WHICH variable an annotation used — which is what pairs a return
    /// type with the parameter that supplies it.
    last_type_param: Option<String>,
    /// The type-parameter names of the `fun` whose body is being parsed
    /// (`fun <T> f(…)`). A runtime test against one of them — `x is T`, `x as T`
    /// — needs a `reified` type argument the coarse type system cannot carry, so
    /// it is rejected here rather than silently answering for a class named `T`
    /// that does not exist.
    type_params: Vec<String>,
    /// The type-parameter names of the enclosing CLASS only, without the ones a
    /// method adds — the list [`crate::ast::ClassDecl::type_params`] keeps.
    ///
    /// `type_params` above is the union (a method body sees both its class's and
    /// its own), and the union cannot be indexed: a receiver's type argument is
    /// positional against the CLASS's list, so `class Box<T> { fun <U> f(): T }`
    /// has to record `0` for `T` whichever position the union put it in.
    class_type_params: Vec<String>,
    /// Set while parsing a supertype's `by` delegate expression, where the `{`
    /// that follows opens the CLASS BODY rather than a trailing lambda —
    /// `class C(g: G) : G by g { override fun … }`. Kotlin resolves the same
    /// ambiguity the same way: a trailing lambda is not allowed there.
    no_trailing_lambda: bool,
    /// Classes synthesized while parsing another declaration, which
    /// [`parse_program`] drains to the top level. An `enum` entry with a body
    /// (`PLUS { override fun apply(…) = … }`) is an anonymous subclass of the
    /// enum, and a subclass is a top-level `class` here; the entry is parsed
    /// deep inside [`Parser::class_decl_mods`], which can only return one
    /// declaration, so the extra ones queue here.
    pending_classes: Vec<ClassDecl>,
}

/// Whether a postfix `(` may apply to this expression as an invocation.
///
/// Only chains that already produced a value *through* a call, index, lambda
/// literal or `!!` qualify. A bare literal or name never does, so `val a = 1`
/// followed by a statement-leading `(1..3)` cannot be misread as `1(1..3)`.
/// A name followed by `(` is a [`Expr::Call`] the primary rule already
/// consumed, so nothing is lost by excluding it here.
fn invocable(e: &Expr) -> bool {
    match e {
        Expr::Call { .. }
        | Expr::Invoke { .. }
        | Expr::Index { .. }
        | Expr::MethodCall { .. }
        | Expr::Lambda { .. } => true,
        Expr::NotNull(inner) => invocable(inner),
        _ => false,
    }
}

/// The coarse type of one function-type parameter: a lone identifier reads as
/// its named type, anything more (generic, nullable, nested function) is
/// `Unknown`.
fn fn_param_ty(words: &[String], plain: bool) -> Type {
    match (plain, words) {
        (true, [one]) => Type::from_name(one),
        _ => Type::Unknown,
    }
}

/// The declaration modifiers kotlinrs recognizes. All of them are Kotlin *soft*
/// keywords, so they lex as plain identifiers and are only meaningful in front
/// of a `fun`/`class`.
#[derive(Debug, Clone, Copy, Default)]
struct Mods {
    open: bool,
    abstract_: bool,
    override_: bool,
    sealed: bool,
}

/// Parse a full program: top-level `fun`, `class`/`data class`, `interface`, and
/// `object` declarations, each optionally preceded by modifiers.
pub fn parse_program(src: &str) -> Result<Program, String> {
    let toks = Lexer::new(src).tokenize()?;
    let mut p = Parser {
        toks,
        pos: 0,
        fn_param_types: Vec::new(),
        fn_ret_types: Vec::new(),
        last_type_param: None,
        type_params: Vec::new(),
        class_type_params: Vec::new(),
        no_trailing_lambda: false,
        pending_classes: Vec::new(),
    };
    let mut prog = Program::default();
    while !p.at(&Tok::Eof) {
        // A modifier run only starts a declaration; anything else keeps its
        // ordinary identifier meaning (an `import`/`package` line, say).
        if !p.at_decl_kw() && matches!(p.peek(), Tok::Ident(w) if is_modifier_word(w)) {
            let mods = p.modifiers();
            if matches!(p.peek(), Tok::Val | Tok::Var) {
                prog.props.push(p.body_prop()?);
                continue;
            }
            if p.at(&Tok::Fun) {
                let f = p.fun_decl_mods(mods)?;
                if f.is_abstract {
                    return Err(format!("top-level fun {} needs a body", f.name));
                }
                prog.funs.push(f);
            } else {
                prog.classes.push(p.class_decl_mods(mods, None)?);
            }
            continue;
        }
        match p.peek() {
            Tok::Fun => {
                let f = p.fun_decl()?;
                if f.is_abstract {
                    return Err(format!("top-level fun {} needs a body", f.name));
                }
                prog.funs.push(f);
            }
            Tok::Val | Tok::Var => prog.props.push(p.body_prop()?),
            Tok::Class | Tok::Data | Tok::Object => prog.classes.push(p.class_decl()?),
            Tok::Ident(w) if w == "interface" => prog.classes.push(p.class_decl()?),
            // `enum class E { … }`. `enum` is a soft keyword, so it is matched
            // positionally — only one directly followed by `class` starts a
            // declaration, leaving a variable or function named `enum` alone.
            Tok::Ident(w) if w == "enum" && matches!(p.peek_at(1), Tok::Class) => {
                prog.classes.push(p.class_decl()?)
            }
            // `package a.b` / `import a.b.*` — a dotted path, optionally ending
            // in `.*`. The package declaration is accepted and discarded (a
            // single-file program has no package-level name resolution here);
            // imports are recorded because Kotlin gates `kotlin.math` names on
            // them (see [`Program::imports`]).
            Tok::Ident(kw) if kw == "import" || kw == "package" => {
                let is_import = kw == "import";
                p.bump();
                let decl = p.dotted_path()?;
                if is_import {
                    prog.imports.push(decl);
                }
            }
            other => {
                return Err(format!(
                    "expected a top-level `fun`, `class`, or `object`, found {other:?} (line {})",
                    p.line()
                ))
            }
        }
    }
    // The subclasses the `enum` entry lowering synthesized are ordinary
    // top-level classes; they are appended before the companion hoist so that
    // an entry-body subclass carrying its own companion is hoisted too.
    prog.classes.append(&mut p.pending_classes);
    // Hoist each `companion object` to the top level. From here on it is an
    // ordinary singleton, and only the owner→companion NAME relation (which
    // `companion_name` reconstructs) is needed to resolve `Owner.member`.
    let hoisted: Vec<ClassDecl> = prog
        .classes
        .iter_mut()
        .filter_map(|cd| cd.companion.take().map(|c| *c))
        .collect();
    prog.classes.extend(hoisted);
    expand_interface_delegation(&mut prog)?;
    Ok(prog)
}

/// One constant of an `enum class`, as written.
struct EnumEntry {
    name: String,
    /// The arguments to the enum's primary constructor, `RED(0xFF0000)`.
    args: Vec<Expr>,
    /// `RED { override fun f() = … }` — the constant's own overrides, which
    /// make it an anonymous subclass of the enum.
    body: Option<Vec<FunDecl>>,
    line: u32,
}

/// The synthetic subclass an `enum` constant with a body becomes. `$` cannot
/// appear in a Kotlin identifier, so the name can never collide with a declared
/// one.
fn entry_class_name(cls: &str, entry: &str) -> String {
    format!("{cls}${entry}")
}

/// A plain (uninterpolated) string literal expression.
fn str_lit(s: &str) -> Expr {
    Expr::Str(vec![StrExpr::Text(s.to_string())])
}

/// The synthetic field a `class C(b: B) : I by b` stores its delegate in.
/// `$` cannot appear in a Kotlin identifier, so it can never collide.
fn delegate_field(iface: &str) -> String {
    format!("$delegate${iface}")
}

/// Rewrite every `class C : I by expr` into ordinary code: a stored property
/// holding the delegate, plus one forwarding method per member of `I` the class
/// does not declare itself.
///
/// Forwarding covers `I`'s methods WITH default bodies too, not just its
/// abstract ones. That is what reproduces Kotlin's semantics: because the
/// default body runs on the delegate, it calls the DELEGATE's implementation of
/// any abstract member, not the delegating class's override —
/// `class D(g: G) : G by g { override fun greet() = "override" }` has
/// `D(impl).greet()` answer `"override"` but `D(impl).twice()` answer the
/// delegate's greeting twice.
fn expand_interface_delegation(prog: &mut Program) -> Result<(), String> {
    // Every interface's members, including the ones it inherits from the
    // interfaces it extends.
    let ifaces: HashMap<String, ClassDecl> = prog
        .classes
        .iter()
        .filter(|c| c.is_interface)
        .map(|c| (c.name.clone(), c.clone()))
        .collect();
    fn members(
        name: &str,
        ifaces: &HashMap<String, ClassDecl>,
        out: &mut Vec<FunDecl>,
        depth: u32,
    ) {
        if depth > 32 {
            return; // a cyclic `interface A : B`, already rejected downstream
        }
        let Some(cd) = ifaces.get(name) else { return };
        for p in &cd.parents {
            members(p, ifaces, out, depth + 1);
        }
        for m in &cd.methods {
            if !out.iter().any(|e| e.name == m.name) {
                out.push(m.clone());
            }
        }
    }

    for cd in &mut prog.classes {
        if cd.delegates.is_empty() {
            continue;
        }
        for (iface, expr) in std::mem::take(&mut cd.delegates) {
            if !ifaces.contains_key(&iface) {
                return Err(format!(
                    "class {}: `by` delegation requires an interface supertype; \
                     {iface} is not one",
                    cd.name
                ));
            }
            let field = delegate_field(&iface);
            // The delegate is stored FIRST so a body property initializer can
            // already see it.
            cd.obj_props.insert(
                0,
                BodyProp {
                    name: field.clone(),
                    ty: Type::Obj,
                    class: None,
                    init: expr,
                    mutable: false,
                    lazy: false,
                    delegate: false,
                },
            );
            let mut inherited = Vec::new();
            members(&iface, &ifaces, &mut inherited, 0);
            for m in inherited {
                if cd.methods.iter().any(|own| own.name == m.name) {
                    continue; // an explicit override in the body wins
                }
                let recv = Expr::Member {
                    recv: Box::new(Expr::Var("this".into())),
                    name: field.clone(),
                    safe: false,
                    line: m.line,
                };
                let call = Expr::MethodCall {
                    recv: Box::new(recv),
                    name: m.name.clone(),
                    args: m.params.iter().map(|p| Expr::Var(p.name.clone())).collect(),
                    safe: false,
                    line: m.line,
                };
                cd.methods.push(FunDecl {
                    name: m.name.clone(),
                    recv: None,
                    params: m.params.clone(),
                    ret: m.ret,
                    ret_class: m.ret_class.clone(),
                    // The forwarder takes the delegate's parameters verbatim, so
                    // the argument that supplies a type-variable result is at
                    // the same index.
                    ret_type_param_of: m.ret_type_param_of,
                    // A CLASS type variable is not carried across: the index is
                    // positional against the interface's type-parameter list,
                    // and the delegating class has a list of its own that need
                    // not agree with it in length or in order. Dropping it
                    // leaves such a call untyped, which is the answer the
                    // frontend already gives everywhere it cannot name a width.
                    ret_class_type_param_of: None,
                    body: vec![Stmt::new(m.line, StmtKind::Return(Some(call)))],
                    line: m.line,
                    is_abstract: false,
                    is_open: true,
                    is_override: true,
                });
            }
        }
    }
    Ok(())
}

/// The spelling of a token that is a *soft* keyword in Kotlin — meaningful only
/// in one syntactic position and an ordinary identifier everywhere else. The
/// lexer gives `until`/`downTo`/`step` (infix range functions) and `data` (a
/// class modifier) dedicated tokens because the range and declaration grammars
/// read them positionally, so an identifier position has to accept them back:
/// `fun step(): Int` and `x.data` are both legal Kotlin.
fn soft_keyword(t: &Tok) -> Option<&'static str> {
    match t {
        Tok::Until => Some("until"),
        Tok::DownTo => Some("downTo"),
        Tok::Step => Some("step"),
        Tok::Data => Some("data"),
        _ => None,
    }
}

/// Whether an identifier spells one of the bitwise infix member functions.
fn is_bitwise_infix(w: &str) -> bool {
    matches!(w, "and" | "or" | "xor" | "shl" | "shr" | "ushr")
}

/// Whether an identifier is one of the declaration modifiers (see [`Mods`]).
fn is_modifier_word(w: &str) -> bool {
    matches!(
        w,
        "open"
            | "abstract"
            | "override"
            | "sealed"
            | "final"
            | "public"
            | "private"
            | "internal"
            | "protected"
            | "inner"
            // Accepted and discarded: each affects how a declaration is COMPILED
            // on the JVM (inlining, tail-call rewriting, call syntax,
            // compile-time constants) without changing what it computes, and
            // this frontend's lowering already differs from the JVM's.
            | "inline"
            | "noinline"
            | "crossinline"
            | "tailrec"
            | "operator"
            | "infix"
            | "const"
    )
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }
    /// The token `n` positions ahead (clamped to `Eof` past the end).
    fn peek_at(&self, n: usize) -> &Tok {
        self.toks
            .get(self.pos + n)
            .map(|s| &s.tok)
            .unwrap_or(&Tok::Eof)
    }
    fn line(&self) -> u32 {
        self.toks[self.pos].line
    }
    fn at(&self, t: &Tok) -> bool {
        self.peek() == t
    }
    /// Whether the current token began on the same source line as the token
    /// just consumed. The lexer drops newlines, so this is the only way to
    /// recover Kotlin's newline sensitivity — needed for postfix `(`, where
    /// `f()\n(1..3).forEach { … }` is two statements but `f()(1)` is one
    /// invocation of the returned function.
    fn glued_to_prev(&self) -> bool {
        self.pos > 0 && self.toks[self.pos - 1].line == self.line()
    }
    /// The token following the `)` that closes the `(` at the cursor, without
    /// consuming anything. Used to tell a function type's parameter list
    /// (`(Int) -> R`) from a parenthesized type (`(() -> Int)`), which differ
    /// only in whether an `->` follows the closing paren.
    fn tok_after_parens(&self) -> &Tok {
        let mut depth = 0usize;
        let mut i = self.pos;
        while let Some(s) = self.toks.get(i) {
            match s.tok {
                Tok::LParen => depth += 1,
                Tok::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return self.toks.get(i + 1).map(|s| &s.tok).unwrap_or(&Tok::Eof);
                    }
                }
                Tok::Eof => break,
                _ => {}
            }
            i += 1;
        }
        &Tok::Eof
    }
    fn bump(&mut self) -> Tok {
        let t = self.toks[self.pos].tok.clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }
    fn eat(&mut self, t: &Tok) -> Result<(), String> {
        if self.at(t) {
            self.bump();
            Ok(())
        } else {
            Err(format!(
                "expected {:?}, found {:?} (line {})",
                t,
                self.peek(),
                self.line()
            ))
        }
    }
    fn ident(&mut self) -> Result<String, String> {
        if let Some(s) = soft_keyword(self.peek()) {
            self.bump();
            return Ok(s.to_string());
        }
        match self.bump() {
            Tok::Ident(s) => Ok(s),
            other => Err(format!("expected identifier, found {:?}", other)),
        }
    }

    /// A dotted name path after `import`/`package`: `a.b.c`, `a.b.*`, or a
    /// renaming `a.b as c`. The lexer discards newlines, so the path ends where
    /// the dots stop.
    fn dotted_path(&mut self) -> Result<ImportDecl, String> {
        let mut path = self.ident()?;
        while self.at(&Tok::Dot) {
            self.bump();
            if self.at(&Tok::Star) {
                self.bump();
                path.push_str(".*");
                return Ok(ImportDecl { path, alias: None });
            }
            path.push('.');
            path.push_str(&self.ident()?);
        }
        let alias = if matches!(self.peek(), Tok::Ident(a) if a == "as") {
            self.bump();
            Some(self.ident()?)
        } else {
            None
        };
        Ok(ImportDecl { path, alias })
    }

    // ── Declarations ───────────────────────────────────────────────

    /// Consume the declaration modifiers that precede a `fun`/`class`. They are
    /// soft keywords in Kotlin — ordinary identifiers everywhere else — so they
    /// arrive as [`Tok::Ident`] and are recognized only in this position. The
    /// visibility modifiers are accepted and discarded: a single-file program has
    /// no visibility boundaries to enforce.
    fn modifiers(&mut self) -> Mods {
        let mut m = Mods::default();
        while let Tok::Ident(w) = self.peek() {
            match w.as_str() {
                "open" => m.open = true,
                "abstract" => m.abstract_ = true,
                "override" => m.override_ = true,
                "sealed" => m.sealed = true,
                "final" | "public" | "private" | "internal" | "protected" | "inner" | "inline"
                | "noinline" | "crossinline" | "tailrec" | "operator" | "infix" | "const" => {}
                _ => break,
            }
            self.bump();
        }
        m
    }

    /// True when the parser is positioned on a declaration keyword — `fun`,
    /// `class`, `data`, `object`, or the soft keyword `interface`.
    fn at_decl_kw(&self) -> bool {
        matches!(self.peek(), Tok::Fun | Tok::Class | Tok::Data | Tok::Object)
            || matches!(self.peek(), Tok::Ident(w) if w == "interface")
    }

    fn fun_decl(&mut self) -> Result<FunDecl, String> {
        self.fun_decl_mods(Mods::default())
    }

    fn fun_decl_mods(&mut self, mods: Mods) -> Result<FunDecl, String> {
        let line = self.line();
        self.eat(&Tok::Fun)?;
        // An optional generic parameter list, `fun <T> f(x: T)`. Coarse typing
        // keeps no type variables, so the list is consumed and only the NAMES
        // are kept — a `T`-typed parameter reads as `Unknown`, which is how the
        // frontend already handles a value it cannot type.
        // A method's own parameters ADD to the enclosing class's rather than
        // replacing them: inside `class Box<T> { fun <U> f(a: T, b: U) }` both
        // `T` and `U` are type variables.
        let mut tps = self.type_params.clone();
        tps.extend(self.type_params_decl());
        let outer_tps = std::mem::replace(&mut self.type_params, tps);
        // `fun Recv.name(…)` — an extension. The first identifier is the
        // receiver type only when a `.` follows it.
        let first = self.ident()?;
        let mut recv = None;
        let name = if self.at(&Tok::Dot) {
            self.bump();
            let (ty, class) = (Type::from_name(&first), None);
            let class = if ty == Type::Unknown {
                Some(first.clone())
            } else {
                class
            };
            recv = Some((
                first,
                if ty == Type::Unknown { Type::Obj } else { ty },
                class,
            ));
            self.ident()?
        } else if self.at(&Tok::Lt) {
            // `fun List<Int>.sum2()` — a generic receiver keeps its head name.
            self.skip_type_args();
            self.eat(&Tok::Dot)?;
            recv = Some((first.clone(), Type::Obj, None));
            self.ident()?
        } else {
            first
        };
        self.eat(&Tok::LParen)?;
        let mut params = Vec::new();
        // The type variable each parameter was declared with, positionally.
        let mut param_tps: Vec<Option<String>> = Vec::new();
        while !self.at(&Tok::RParen) {
            let is_vararg = matches!(self.peek(), Tok::Ident(w) if w == "vararg");
            if is_vararg {
                self.bump();
            }
            let pname = self.ident()?;
            let (ty, class) = if self.at(&Tok::Colon) {
                self.bump();
                self.type_ref()?
            } else {
                self.last_type_param = None;
                (Type::Unknown, None)
            };
            // A `vararg T` arrives as an array, so the result of a call is not
            // the parameter's own type — it cannot supply a type argument.
            param_tps.push(if is_vararg {
                None
            } else {
                self.last_type_param.take()
            });
            // A default value, `fun f(a: Int, b: Int = 10)`.
            let default = if self.at(&Tok::Assign) {
                self.bump();
                Some(self.expr()?)
            } else {
                None
            };
            params.push(Param {
                name: pname,
                // A `vararg` parameter arrives as an array of the declared
                // element type, so its own coarse type is a heap object; the
                // element type rides in `vararg` for the body's `for` loop.
                ty: if is_vararg { Type::Obj } else { ty },
                class,
                default,
                vararg: is_vararg.then_some(ty),
            });
            if self.at(&Tok::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.eat(&Tok::RParen)?;
        let (ret_annot, ret_class, ret_tp) = if self.at(&Tok::Colon) {
            self.bump();
            let (t, c) = self.type_ref()?;
            (Some(t), c, self.last_type_param.take())
        } else {
            (None, None, None)
        };
        // Pair the result's type variable with the parameter that carries it.
        let ret_type_param_of = ret_tp.as_ref().and_then(|r| {
            param_tps
                .iter()
                .position(|p| p.as_deref() == Some(r.as_str()))
        });
        // …and, failing that, with the enclosing class's type parameter of that
        // name: `class Box<T>(val v: T) { fun get(): T = v }` reads its width
        // from the RECEIVER's type argument rather than from an argument of its
        // own. Both can hold at once (`fun id(x: T): T` inside `Box<T>`); the
        // argument is checked first at the call site because it is present even
        // where the receiver's instantiation is not known.
        let ret_class_type_param_of = ret_tp.and_then(|r| {
            self.class_type_params
                .iter()
                .position(|p| p.as_str() == r.as_str())
        });
        // Body is either a block `{ … }` or a single-expression body `= expr`
        // (Kotlin `fun f(...) = expr`), which desugars to `{ return expr }`.
        // An `abstract`/`interface` member has NO body: the declaration ends
        // right there, and the next token opens the following member (or closes
        // the class body).
        let bodyless = !self.at(&Tok::Assign) && !self.at(&Tok::LBrace);
        let (body, is_expr_body) = if bodyless {
            (Vec::new(), false)
        } else if self.at(&Tok::Assign) {
            self.bump();
            let e = self.expr()?;
            (vec![Stmt::new(line, StmtKind::Return(Some(e)))], true)
        } else {
            (self.block()?, false)
        };
        // With no explicit return annotation, a block body defaults to `Unit`
        // (its value is discarded); an `= expr` body's type is the expression's,
        // which the frontend doesn't fully infer — leave it `Unknown` so callers
        // lower conservatively rather than being forced to `Unit`.
        let ret = match ret_annot {
            Some(t) => t,
            None if is_expr_body => Type::Unknown,
            None => Type::Unit,
        };
        self.type_params = outer_tps;
        Ok(FunDecl {
            name,
            recv,
            params,
            ret,
            ret_class,
            ret_type_param_of,
            ret_class_type_param_of,
            body,
            line,
            is_abstract: bodyless,
            is_open: mods.open,
            is_override: mods.override_,
        })
    }

    /// A `class C(...)`, `data class C(...)`, `object O { ... }`, or
    /// `interface I { ... }`, with the modifiers already consumed by
    /// [`Parser::modifiers`].
    fn class_decl(&mut self) -> Result<ClassDecl, String> {
        self.class_decl_mods(Mods::default(), None)
    }

    /// `companion_of` names the enclosing class when this is a `companion
    /// object`: the declaration is then an `object` whose name is synthesized
    /// from the owner, because the companion may be written without one.
    fn class_decl_mods(
        &mut self,
        mods: Mods,
        companion_of: Option<&str>,
    ) -> Result<ClassDecl, String> {
        let line = self.line();
        let is_data = if self.at(&Tok::Data) {
            self.bump();
            true
        } else {
            false
        };
        // `enum class E(…) { A, B; … }`. A soft keyword, matched positionally so
        // only an `enum` directly in front of `class` is one.
        let is_enum = matches!(self.peek(), Tok::Ident(w) if w == "enum")
            && matches!(self.peek_at(1), Tok::Class);
        if is_enum {
            self.bump();
        }
        let is_interface = matches!(self.peek(), Tok::Ident(w) if w == "interface");
        let is_object = if is_interface {
            self.bump();
            false
        } else if self.at(&Tok::Object) {
            if is_data {
                return Err("`data object` is not supported".into());
            }
            self.bump();
            true
        } else {
            self.eat(&Tok::Class)?;
            false
        };
        let name = match companion_of {
            // `companion object { … }` may be anonymous, and a named one
            // (`companion object Factory`) is still reached through the owner —
            // so either way the declaration is hoisted under the owner's name.
            Some(owner) => {
                if matches!(self.peek(), Tok::Ident(_)) {
                    self.bump();
                }
                companion_name(owner)
            }
            None => self.ident()?,
        };
        // A generic class keeps only its head name; the coarse type system
        // carries no type variables. The parameter NAMES are still recorded, so
        // an annotation that mentions one (`val v: T`) resolves to the unknown
        // type rather than to a heap class called `T` — see [`Parser::type_ref`].
        let tps = self.type_params_decl();
        let outer_ctps = std::mem::replace(&mut self.class_type_params, tps.clone());
        let outer_tps = std::mem::replace(&mut self.type_params, tps.clone());

        // Primary constructor (classes only). `object`s and `interface`s have
        // none.
        let mut params = Vec::new();
        let has_primary = !is_object && !is_interface && self.at(&Tok::LParen);
        if has_primary {
            self.bump();
            while !self.at(&Tok::RParen) {
                // A constructor property carries modifiers like any other:
                // `class Base(override val name: String)` is how a class
                // satisfies an interface's declared property with storage, and
                // `private val` is just as legal.
                self.modifiers();
                let kind = match self.peek() {
                    Tok::Val => {
                        self.bump();
                        PropKind::Val
                    }
                    Tok::Var => {
                        self.bump();
                        PropKind::Var
                    }
                    _ => PropKind::None,
                };
                let pname = self.ident()?;
                self.eat(&Tok::Colon)?;
                let (ty, class) = self.type_ref()?;
                // The type variable this parameter was declared with, as an
                // index into the class's list — what a construction site reads
                // the type argument off (see [`CtorProp::type_param_of`]).
                let type_param_of = self.last_type_param.take().and_then(|r| {
                    self.class_type_params
                        .iter()
                        .position(|p| p.as_str() == r.as_str())
                });
                let default = if self.at(&Tok::Assign) {
                    self.bump();
                    Some(self.expr()?)
                } else {
                    None
                };
                params.push(CtorProp {
                    name: pname,
                    ty,
                    class,
                    kind,
                    default,
                    type_param_of,
                });
                if self.at(&Tok::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
            self.eat(&Tok::RParen)?;
        }
        // Every enum constant carries the two properties `Enum` declares —
        // `name` and `ordinal`. They are appended to the primary constructor so
        // the ordinary class lowering stores them like any other `val`, and the
        // entry lowering below is what supplies their arguments; the constants
        // are the only construction sites, so a user never sees the extra
        // parameters. Appending (rather than prepending) keeps the DECLARED
        // parameters at the positions an entry's argument list writes them.
        let has_primary = has_primary || is_enum;
        if is_enum {
            for (pname, ty) in [("name", Type::String), ("ordinal", Type::Int)] {
                if params.iter().any(|p| p.name == pname) {
                    return Err(format!(
                        "enum class {name}: `{pname}` is already declared by Enum"
                    ));
                }
                params.push(CtorProp {
                    name: pname.to_string(),
                    ty,
                    class: None,
                    kind: PropKind::Val,
                    default: None,
                    // The two properties `Enum` declares are not generic.
                    type_param_of: None,
                });
            }
        }
        if is_data && params.iter().all(|p| p.kind == PropKind::None) {
            return Err(format!(
                "data class {name} needs at least one `val`/`var` constructor property"
            ));
        }

        // Supertype list: `: Super(args), Iface1, Iface2`. Only the *first*
        // entry may carry constructor arguments — Kotlin's own rule, since a
        // class has exactly one superclass and interfaces have no constructor.
        let mut parents = Vec::new();
        let mut super_args = Vec::new();
        let mut delegates: Vec<(String, Expr)> = Vec::new();
        if self.at(&Tok::Colon) {
            self.bump();
            loop {
                let pname = self.ident()?;
                // A generic supertype (`Comparable<T>`) keeps only its head name.
                self.skip_type_args();
                if self.at(&Tok::LParen) {
                    if !parents.is_empty() {
                        return Err(format!(
                            "class {name}: only the superclass may take constructor arguments \
                             (line {})",
                            self.line()
                        ));
                    }
                    self.bump();
                    while !self.at(&Tok::RParen) {
                        super_args.push(self.expr()?);
                        if self.at(&Tok::Comma) {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    self.eat(&Tok::RParen)?;
                }
                // `: I by expr` — interface delegation. Every member of `I`
                // the class does not itself declare is forwarded to the value
                // of `expr`.
                if matches!(self.peek(), Tok::Ident(w) if w == "by") {
                    self.bump();
                    self.no_trailing_lambda = true;
                    let d = self.expr();
                    self.no_trailing_lambda = false;
                    delegates.push((pname.clone(), d?));
                }
                parents.push(pname);
                if self.at(&Tok::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }

        // Body: methods, plus (for `object`) property initializers.
        let mut methods = Vec::new();
        let mut obj_props = Vec::new();
        let mut companion = None;
        let mut inits: Vec<InitBlock> = Vec::new();
        let mut secondaries: Vec<SecondaryCtor> = Vec::new();
        let mut abstract_props: Vec<CtorProp> = Vec::new();
        let mut entries: Vec<EnumEntry> = Vec::new();
        if self.at(&Tok::LBrace) {
            self.bump();
            // An enum body opens with its constants, comma-separated, and the
            // members (if any) follow a `;`. The constants come first in the
            // grammar, so they are consumed before the member loop rather than
            // being distinguished from a member by lookahead.
            if is_enum {
                entries = self.enum_entries()?;
            }
            while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
                if self.at(&Tok::Semi) {
                    self.bump();
                    continue;
                }
                // `companion object [Name] { … }` — one per class.
                if matches!(self.peek(), Tok::Ident(w) if w == "companion")
                    && matches!(self.peek_at(1), Tok::Object)
                {
                    self.bump();
                    if companion.is_some() {
                        return Err(format!(
                            "class {name}: only one companion object is allowed"
                        ));
                    }
                    companion = Some(Box::new(
                        self.class_decl_mods(Mods::default(), Some(&name))?,
                    ));
                    continue;
                }
                // `init { … }` — an initializer block. A soft keyword, so it is
                // matched positionally: only an `init` directly followed by `{`
                // is one, leaving a property or method named `init` alone.
                if matches!(self.peek(), Tok::Ident(w) if w == "init")
                    && matches!(self.peek_at(1), Tok::LBrace)
                {
                    if is_interface {
                        return Err(format!(
                            "interface {name}: an `init` block needs a constructor; \
                             interfaces have none"
                        ));
                    }
                    self.bump(); // `init`
                                 // The shared block rule, so an `init` body accepts `;`
                                 // separators and everything else a `fun` body does.
                    let body = self.block()?;
                    inits.push(InitBlock {
                        after_props: obj_props.len(),
                        body,
                    });
                    continue;
                }
                // `constructor(…) [: this(…) | : super(…)] { … }` — a secondary
                // constructor.
                if matches!(self.peek(), Tok::Ident(w) if w == "constructor") {
                    if is_interface || is_object {
                        return Err(format!(
                            "{name}: a secondary constructor needs a constructor; \
                             an interface and an object have none"
                        ));
                    }
                    secondaries.push(self.secondary_ctor()?);
                    continue;
                }
                let mods = self.modifiers();
                match self.peek() {
                    Tok::Fun => methods.push(self.fun_decl_mods(mods)?),
                    // A body property: `val n = expr` / `var c: Int = expr`, in a
                    // class as well as an `object`. An `interface` has no
                    // storage to put one in, so it is rejected there.
                    Tok::Val | Tok::Var => {
                        // `val x: T get() = …` is a computed property: a
                        // zero-argument method wearing property syntax, which is
                        // what it lowers to. An interface may carry one too —
                        // it has a body, so it needs no storage.
                        if let Some(f) = self.accessor_prop(mods)? {
                            methods.push(f);
                            continue;
                        }
                        // `val x: T` with nothing after it declares the property
                        // WITHOUT storage — the only form an `interface` can
                        // carry, and what `abstract val` means in a class. A
                        // form that does need storage still has none to go in
                        // inside an interface, so that stays an error.
                        if let Some(p) = self.abstract_prop()? {
                            abstract_props.push(p);
                            continue;
                        }
                        if is_interface {
                            return Err(format!(
                                "interface {name}: a property with an initializer needs storage; \
                                 interfaces have none"
                            ));
                        }
                        obj_props.push(self.body_prop()?);
                    }
                    other => {
                        return Err(format!(
                            "class {name}: expected `fun` or a property, found {other:?} (line {})",
                            self.line()
                        ))
                    }
                }
            }
            self.eat(&Tok::RBrace)?;
        }

        // An enum with an abstract member is itself abstract (and Kotlin then
        // requires every constant to carry a body, which the missing
        // constructor reports); an enum any of whose constants carries a body is
        // extended by that constant's synthetic subclass, so it must be open.
        let is_abstract = mods.abstract_ || (is_enum && methods.iter().any(|m| m.is_abstract));
        let is_open = mods.open || (is_enum && entries.iter().any(|e| e.body.is_some()));
        if is_enum {
            // The constants, `values()`, `valueOf` and `entries` all live on the
            // enum's companion, which is how `E.RED` already resolves — an enum
            // may also declare a companion of its own, so they are MERGED into
            // it rather than replacing it.
            companion = Some(Box::new(
                self.lower_enum_entries(&name, entries, &params, companion, line)?,
            ));
        }

        self.type_params = outer_tps;
        self.class_type_params = outer_ctps;
        Ok(ClassDecl {
            name,
            type_params: tps,
            params,
            obj_props,
            methods,
            is_data,
            is_object,
            is_interface,
            is_abstract,
            is_open,
            is_sealed: mods.sealed,
            parents,
            super_args,
            delegates,
            companion,
            has_primary,
            inits,
            secondaries,
            abstract_props,
            is_enum,
            line,
        })
    }

    /// The constant list at the head of an `enum class` body:
    /// `RED, GREEN(2), BLUE { override fun f() = … }` with an optional trailing
    /// `,` and a `;` before the members.
    ///
    /// The list may be empty (`enum class E { ; fun f() = 1 }`, and the
    /// degenerate `enum class E {}`), so a body that opens on `;` or `}` is not
    /// an error.
    fn enum_entries(&mut self) -> Result<Vec<EnumEntry>, String> {
        let mut entries = Vec::new();
        while matches!(self.peek(), Tok::Ident(_)) {
            let line = self.line();
            let name = self.ident()?;
            let mut args = Vec::new();
            if self.at(&Tok::LParen) {
                self.bump();
                while !self.at(&Tok::RParen) {
                    args.push(self.expr()?);
                    if self.at(&Tok::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::RParen)?;
            }
            // `RED { … }` — the constant's own body, an anonymous subclass of
            // the enum. Only its methods are taken: Kotlin allows a property
            // there too, but one would need storage on the subclass, and
            // rejecting is better than dropping it silently.
            let body = if self.at(&Tok::LBrace) {
                self.bump();
                let mut ms = Vec::new();
                while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
                    if self.at(&Tok::Semi) {
                        self.bump();
                        continue;
                    }
                    let mods = self.modifiers();
                    if !self.at(&Tok::Fun) {
                        return Err(format!(
                            "enum constant {name}: only a `fun` may be overridden in a \
                             constant's body (line {})",
                            self.line()
                        ));
                    }
                    ms.push(self.fun_decl_mods(mods)?);
                }
                self.eat(&Tok::RBrace)?;
                Some(ms)
            } else {
                None
            };
            entries.push(EnumEntry {
                name,
                args,
                body,
                line,
            });
            if self.at(&Tok::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        if self.at(&Tok::Semi) {
            self.bump();
        }
        Ok(entries)
    }

    /// Lower an `enum class`'s constants onto its companion object.
    ///
    /// Each constant becomes a `val` on the companion holding one instance, so
    /// `E.RED` resolves through the companion rewrite every `Owner.member`
    /// already uses, and the constant is a SINGLETON — evaluated once when the
    /// companion initializes, which is what makes `E.RED === E.RED` and the
    /// `when (c) { E.RED -> … }` comparison come out right.
    ///
    /// Alongside them go the three members `Enum`'s companion contributes:
    /// `values()` (a fresh array per call, as on the JVM), `entries` (the
    /// standing list), and `valueOf` (which throws `IllegalArgumentException`
    /// for an unknown name, with the JVM's exact message).
    fn lower_enum_entries(
        &mut self,
        cls: &str,
        entries: Vec<EnumEntry>,
        params: &[CtorProp],
        companion: Option<Box<ClassDecl>>,
        line: u32,
    ) -> Result<ClassDecl, String> {
        // Everything the constants supply beyond their written arguments: the
        // declared parameters minus the two `Enum` ones this lowering appended.
        let declared = params.len().saturating_sub(2);
        let mut comp = match companion {
            Some(c) => *c,
            None => ClassDecl {
                name: companion_name(cls),
                // An `enum class` cannot declare type parameters.
                type_params: Vec::new(),
                params: Vec::new(),
                obj_props: Vec::new(),
                methods: Vec::new(),
                is_data: false,
                is_object: true,
                is_interface: false,
                is_abstract: false,
                is_open: false,
                is_sealed: false,
                parents: Vec::new(),
                super_args: Vec::new(),
                delegates: Vec::new(),
                companion: None,
                has_primary: false,
                inits: Vec::new(),
                secondaries: Vec::new(),
                abstract_props: Vec::new(),
                is_enum: false,
                line,
            },
        };

        // The constants are prepended so they initialize before anything the
        // user's own companion declares — a companion property may read `E.RED`,
        // but a constant can never read a user property.
        let mut props = Vec::with_capacity(entries.len() + 1);
        for (ordinal, e) in entries.iter().enumerate() {
            if e.args.len() != declared {
                return Err(format!(
                    "enum constant {cls}.{} takes {} argument(s), but {} were given (line {})",
                    e.name,
                    declared,
                    e.args.len(),
                    e.line
                ));
            }
            // `E(<written args>, "NAME", ordinal)` — or, for a constant with a
            // body, `E$NAME()`, whose synthetic subclass forwards the same
            // arguments to `E`'s constructor.
            let mut args = e.args.clone();
            args.push(str_lit(&e.name));
            args.push(Expr::Int(ordinal as i64));
            let init = match &e.body {
                None => Expr::Call {
                    name: cls.to_string(),
                    args,
                    line: e.line,
                },
                Some(methods) => {
                    let sub = entry_class_name(cls, &e.name);
                    self.pending_classes.push(ClassDecl {
                        name: sub.clone(),
                        type_params: Vec::new(),
                        params: Vec::new(),
                        obj_props: Vec::new(),
                        methods: methods.clone(),
                        is_data: false,
                        is_object: false,
                        is_interface: false,
                        is_abstract: false,
                        is_open: false,
                        is_sealed: false,
                        parents: vec![cls.to_string()],
                        super_args: args,
                        delegates: Vec::new(),
                        companion: None,
                        has_primary: false,
                        inits: Vec::new(),
                        secondaries: Vec::new(),
                        abstract_props: Vec::new(),
                        // The subclass IS the constant, so it displays and
                        // orders as one.
                        is_enum: true,
                        line: e.line,
                    });
                    Expr::Call {
                        name: sub,
                        args: Vec::new(),
                        line: e.line,
                    }
                }
            };
            props.push(BodyProp {
                name: e.name.clone(),
                ty: Type::Obj,
                class: Some(cls.to_string()),
                init,
                mutable: false,
                lazy: false,
                delegate: false,
            });
        }

        // `E.RED`, referenced the way user code does, so the constants are read
        // back through the same companion rewrite rather than a second path.
        let refs: Vec<Expr> = entries
            .iter()
            .map(|e| Expr::Member {
                recv: Box::new(Expr::Var(cls.to_string())),
                name: e.name.clone(),
                safe: false,
                line,
            })
            .collect();

        props.push(BodyProp {
            name: "entries".to_string(),
            ty: Type::Obj,
            class: None,
            init: Expr::Call {
                name: "listOf".to_string(),
                args: refs.clone(),
                line,
            },
            mutable: false,
            lazy: false,
            delegate: false,
        });
        props.append(&mut comp.obj_props);
        comp.obj_props = props;

        // `fun values() = arrayOf(E.RED, …)` — an ARRAY, and a fresh one per
        // call, which is what the JVM's generated `values()` returns.
        comp.methods.push(FunDecl {
            name: "values".to_string(),
            recv: None,
            params: Vec::new(),
            ret: Type::Obj,
            ret_class: None,
            // A synthesized member of a concrete enum: its result is an array of
            // that enum, never a type variable.
            ret_type_param_of: None,
            ret_class_type_param_of: None,
            body: vec![Stmt::new(
                line,
                StmtKind::Return(Some(Expr::Call {
                    name: "arrayOf".to_string(),
                    args: refs,
                    line,
                })),
            )],
            line,
            is_abstract: false,
            is_open: false,
            is_override: false,
        });

        // `fun valueOf(value: String) = when (value) { "RED" -> E.RED; …
        //   else -> throw IllegalArgumentException("No enum constant E.$value") }`
        let subject = "value";
        let mut arms: Vec<WhenArm> = entries
            .iter()
            .map(|e| WhenArm {
                guard: WhenGuard::Conds(vec![WhenCond::Expr(str_lit(&e.name))]),
                body: vec![Stmt::new(
                    line,
                    StmtKind::Expr(Expr::Member {
                        recv: Box::new(Expr::Var(cls.to_string())),
                        name: e.name.clone(),
                        safe: false,
                        line,
                    }),
                )],
            })
            .collect();
        arms.push(WhenArm {
            guard: WhenGuard::Else,
            body: vec![Stmt::new(
                line,
                StmtKind::Expr(Expr::Throw(Box::new(Expr::Call {
                    name: "IllegalArgumentException".to_string(),
                    args: vec![Expr::Str(vec![
                        StrExpr::Text(format!("No enum constant {cls}.")),
                        StrExpr::Expr(Box::new(Expr::Var(subject.to_string()))),
                    ])],
                    line,
                }))),
            )],
        });
        comp.methods.push(FunDecl {
            name: "valueOf".to_string(),
            recv: None,
            params: vec![Param {
                name: subject.to_string(),
                ty: Type::String,
                class: None,
                default: None,
                vararg: None,
            }],
            ret: Type::Obj,
            ret_class: Some(cls.to_string()),
            // Likewise: `valueOf` answers the enum itself.
            ret_type_param_of: None,
            ret_class_type_param_of: None,
            body: vec![Stmt::new(
                line,
                StmtKind::Return(Some(Expr::When(WhenExpr {
                    subject: Some(Box::new(Expr::Var(subject.to_string()))),
                    binding: None,
                    arms,
                    line,
                }))),
            )],
            line,
            is_abstract: false,
            is_open: false,
            is_override: false,
        });

        Ok(comp)
    }

    /// A secondary constructor: `constructor(p: T) : this(a) { … }`.
    ///
    /// The parameter list is a function's minus `vararg` (Kotlin allows it, but
    /// nothing here needs it yet and rejecting is better than mis-binding), and
    /// the body is optional — `constructor(x: Int) : this(x, 0)` is complete.
    fn secondary_ctor(&mut self) -> Result<SecondaryCtor, String> {
        let line = self.line();
        self.bump(); // `constructor`
        self.eat(&Tok::LParen)?;
        let mut params = Vec::new();
        while !self.at(&Tok::RParen) {
            let pname = self.ident()?;
            self.eat(&Tok::Colon)?;
            let (ty, class) = self.type_ref()?;
            let default = if self.at(&Tok::Assign) {
                self.bump();
                Some(self.expr()?)
            } else {
                None
            };
            params.push(Param {
                name: pname,
                ty,
                class,
                default,
                vararg: None,
            });
            if self.at(&Tok::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.eat(&Tok::RParen)?;
        let deleg = if self.at(&Tok::Colon) {
            self.bump();
            let kw = self.ident()?;
            let is_super = match kw.as_str() {
                "super" => true,
                "this" => false,
                other => {
                    return Err(format!(
                        "secondary constructor: expected `this(…)` or `super(…)` after `:`, \
                         found `{other}` (line {line})"
                    ))
                }
            };
            self.eat(&Tok::LParen)?;
            let mut args = Vec::new();
            while !self.at(&Tok::RParen) {
                args.push(self.expr()?);
                if self.at(&Tok::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
            self.eat(&Tok::RParen)?;
            Some(CtorDelegation { is_super, args })
        } else {
            None
        };
        let body = if self.at(&Tok::LBrace) {
            self.block()?
        } else {
            Vec::new()
        };
        Ok(SecondaryCtor {
            params,
            deleg,
            body,
            line,
        })
    }

    /// A stored property with an initializer: `val n: Int = 5`, `var c = 0`, or
    /// the delegated form `val z: Int by lazy { … }`. Used for a class body, an
    /// `object` body, and the top level alike — the three differ in WHERE the
    /// initializer runs, not in how it is written.
    /// `val x: T get() = expr` / `val x: T get() { … }` — a property with a
    /// custom getter and no backing field.
    ///
    /// Kotlin computes such a property on every read, so it is a zero-argument
    /// METHOD wearing property syntax, and that is what it lowers to. Reads
    /// already reach it: `compile_member` resolves a method of the name before a
    /// stored property, so `x.label` runs the getter — and a class implementing
    /// an interface's declared property this way dispatches virtually like any
    /// other override.
    ///
    /// Answers `None` (position untouched) for every other property form.
    fn accessor_prop(&mut self, mods: Mods) -> Result<Option<FunDecl>, String> {
        let start = self.pos;
        let line = self.line();
        self.bump(); // `val` / `var`
        let parsed = (|| -> Result<Option<(String, Type, Option<String>)>, String> {
            let name = self.ident()?;
            if !self.at(&Tok::Colon) {
                return Ok(None);
            }
            self.bump();
            let (ty, class) = self.type_ref()?;
            Ok(Some((name, ty, class)))
        })();
        // The type variable the annotation named, read while it is still fresh
        // (only the `Ok(Some(_))` path below ran `type_ref`, and the others
        // return without using this).
        let ret_class_type_param_of = self.last_type_param.take().and_then(|r| {
            self.class_type_params
                .iter()
                .position(|p| p.as_str() == r.as_str())
        });
        let is_get = matches!(self.peek(), Tok::Ident(w) if w == "get")
            && matches!(self.peek_at(1), Tok::LParen);
        let Ok(Some((name, ty, class))) = parsed else {
            self.pos = start;
            return Ok(None);
        };
        if !is_get {
            self.pos = start;
            return Ok(None);
        }
        self.bump(); // `get`
        self.eat(&Tok::LParen)?;
        self.eat(&Tok::RParen)?;
        let body = if self.at(&Tok::Assign) {
            self.bump();
            let e = self.expr()?;
            vec![Stmt::new(line, StmtKind::Return(Some(e)))]
        } else {
            self.block()?
        };
        // A `set(value) { … }` would need a settable non-field property, which
        // has no lowering here; rejecting is better than silently dropping the
        // writes.
        if matches!(self.peek(), Tok::Ident(w) if w == "set") {
            return Err(format!(
                "property {name}: a custom `set` accessor is not supported (line {})",
                self.line()
            ));
        }
        Ok(Some(FunDecl {
            name,
            recv: None,
            params: Vec::new(),
            ret: ty,
            ret_class: class,
            // A computed property's getter takes no arguments, so an ARGUMENT
            // can never supply its type variable — but the receiver can, when
            // the variable is one of the enclosing class's.
            ret_type_param_of: None,
            ret_class_type_param_of,
            body,
            line,
            is_abstract: false,
            is_open: mods.open,
            is_override: mods.override_,
        }))
    }

    /// `val x: T` / `var x: T` with no initializer, no delegate and no accessor
    /// — a property DECLARATION with no storage. Answers `None` (leaving the
    /// position untouched) for every other property form, so the caller can fall
    /// through to [`Parser::body_prop`].
    ///
    /// The shape can only be recognized by what FOLLOWS the type, and a type is
    /// an arbitrary number of tokens, so the type is parsed and the position
    /// rewound when it turns out not to be one of these.
    fn abstract_prop(&mut self) -> Result<Option<CtorProp>, String> {
        let start = self.pos;
        let kind = match self.bump() {
            Tok::Var => PropKind::Var,
            _ => PropKind::Val,
        };
        // An untyped declaration cannot be abstract: with no initializer there
        // would be nothing to take the type from.
        let parsed = (|| -> Result<Option<(String, Type, Option<String>)>, String> {
            let name = self.ident()?;
            if !self.at(&Tok::Colon) {
                return Ok(None);
            }
            self.bump();
            let (ty, class) = self.type_ref()?;
            Ok(Some((name, ty, class)))
        })();
        // `=` (an initializer), `by` (a delegate) and `get`/`set` (an accessor)
        // each make the property something other than a bare declaration.
        let bare = matches!(parsed, Ok(Some(_)))
            && !self.at(&Tok::Assign)
            && !matches!(self.peek(), Tok::Ident(w) if w == "by" || w == "get" || w == "set");
        match parsed {
            Ok(Some((name, ty, class))) if bare => Ok(Some(CtorProp {
                name,
                ty,
                class,
                kind,
                default: None,
                // An abstract or interface property declaration owns no
                // storage, so no construction site fixes a type argument
                // through it.
                type_param_of: None,
            })),
            _ => {
                self.pos = start;
                Ok(None)
            }
        }
    }

    fn body_prop(&mut self) -> Result<BodyProp, String> {
        let mutable = matches!(self.bump(), Tok::Var);
        let name = self.ident()?;
        let (ty, class) = if self.at(&Tok::Colon) {
            self.bump();
            self.type_ref()?
        } else {
            (Type::Unknown, None)
        };
        // `by lazy { … }`. `by` is a soft keyword, so it only means delegation
        // in this position.
        if matches!(self.peek(), Tok::Ident(w) if w == "by") {
            self.bump();
            // `by lazy { … }` has its own lowering (a cell forced on first
            // read); every other delegate goes through the general
            // `getValue`/`setValue` operator protocol.
            if matches!(self.peek(), Tok::Ident(w) if w == "lazy")
                && matches!(self.peek_at(1), Tok::LBrace)
            {
                self.bump();
                if mutable {
                    return Err(format!("property {name}: `by lazy` requires `val`"));
                }
                let init = self.lambda()?;
                return Ok(BodyProp {
                    name,
                    ty,
                    class,
                    init,
                    mutable,
                    lazy: true,
                    delegate: false,
                });
            }
            let init = self.expr()?;
            return Ok(BodyProp {
                name,
                ty,
                class,
                init,
                mutable,
                lazy: false,
                delegate: true,
            });
        }
        self.eat(&Tok::Assign)?;
        let init = self.expr()?;
        Ok(BodyProp {
            name,
            ty,
            class,
            init,
            mutable,
            lazy: false,
            delegate: false,
        })
    }

    /// Consume a `<…>` type-PARAMETER list if one is present, returning the
    /// declared names. `<reified T>`/`<in T>`/`<T : Comparable<T>>` all reduce to
    /// their bare names here; the bound and the variance carry no meaning for a
    /// coarse type system.
    fn type_params_decl(&mut self) -> Vec<String> {
        let start = self.pos;
        if !self.at(&Tok::Lt) {
            return Vec::new();
        }
        self.skip_type_args();
        let mut names = Vec::new();
        let mut depth = 0i32;
        let mut want = true;
        for i in start..self.pos {
            match &self.toks[i].tok {
                Tok::Lt => {
                    depth += 1;
                    want = depth == 1;
                }
                Tok::Gt => depth -= 1,
                Tok::Comma if depth == 1 => want = true,
                // A modifier or a bound is not the parameter's own name.
                Tok::Ident(w) if want && depth == 1 => {
                    if !matches!(w.as_str(), "reified" | "in" | "out") {
                        names.push(w.clone());
                        want = false;
                    }
                }
                _ => want = false,
            }
        }
        names
    }

    /// Consume a `<…>` type-argument list if one is present, discarding it.
    /// Coarse typing keeps only the head name of a generic supertype.
    fn skip_type_args(&mut self) {
        if !self.at(&Tok::Lt) {
            return;
        }
        let mut depth = 0;
        loop {
            match self.bump() {
                Tok::Lt => depth += 1,
                Tok::Gt => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                Tok::Eof => return,
                _ => {}
            }
        }
    }

    /// A type reference — `Int`, `String`, `Array<String>`, `Int?`, … Generic
    /// args are consumed but ignored (coarse typing), and a trailing `?`
    /// (nullable) is accepted and discarded — nullability is tracked at the
    /// value/flow level, not in the coarse static type.
    fn type_name(&mut self) -> Result<Type, String> {
        Ok(self.type_ref()?.0)
    }

    /// Like [`Parser::type_name`], but also returns the raw type name when it is
    /// not a builtin primitive — a heap-object type (`Type::Obj`) whose class /
    /// container name the compiler needs for method dispatch (`Person`,
    /// `List`, `Map`, …).
    fn type_ref(&mut self) -> Result<(Type, Option<String>), String> {
        // A function type `(T1, …) -> R` (chainable: `(Int) -> (Int) -> Int`).
        // The coarse type can't carry a signature, so the parameter/return types
        // are consumed and discarded; the annotation only needs to mark the
        // binding as a callable value (its lowering is closure-invoke by slot).
        // A parenthesized type `(T)` — which is what the return type of
        // `() -> (() -> Int)` is. Only a `(…)` followed by `->` is a parameter
        // list; without the arrow the parens are grouping, so the inner type is
        // parsed on its own. Distinguished by looking past the matching `)`
        // rather than by backtracking, because the parameter-list scan below
        // has the `fn_param_types` side channel it cannot cleanly undo.
        if self.at(&Tok::LParen) && !matches!(self.tok_after_parens(), Tok::Arrow) {
            self.bump();
            let inner = self.type_ref()?;
            self.eat(&Tok::RParen)?;
            if self.at(&Tok::Question) {
                self.bump(); // nullable parenthesized type `((Int) -> Int)?`
            }
            return Ok(inner);
        }
        if self.at(&Tok::LParen) {
            self.bump();
            // Record the parameter types as the list goes by. A parameter is
            // typed only when it is ONE plain identifier (`Int`, `String`); a
            // generic, nullable, or nested function type widens to `Unknown`,
            // which is the same answer the coarse type would have given.
            let mut depth = 1i32;
            let mut params: Vec<Type> = Vec::new();
            let mut words: Vec<String> = Vec::new();
            let mut plain = true;
            let mut any = false;
            loop {
                let tok = self.peek().clone();
                match tok {
                    Tok::RParen if depth == 1 => {
                        if any || !words.is_empty() {
                            params.push(fn_param_ty(&words, plain));
                        }
                        self.bump();
                        break;
                    }
                    Tok::Comma if depth == 1 => {
                        params.push(fn_param_ty(&words, plain));
                        words.clear();
                        plain = true;
                        any = true;
                        self.bump();
                    }
                    Tok::Eof => return Err("unterminated function type".into()),
                    _ => {
                        match &tok {
                            Tok::LParen | Tok::Lt => {
                                depth += 1;
                                plain = false;
                            }
                            Tok::RParen | Tok::Gt => depth -= 1,
                            Tok::Ident(n) if depth == 1 => words.push(n.clone()),
                            _ if depth == 1 => plain = false,
                            _ => {}
                        }
                        self.bump();
                    }
                }
            }
            self.fn_param_types.extend(params);
            self.eat(&Tok::Arrow)?;
            // The return type (which may itself be a function type). Published
            // on the side channel AFTER the recursive call, so a nested arrow
            // leaves the outermost result last — see `fn_ret_types`.
            let ret = self.type_ref()?;
            self.fn_ret_types.push(ret.0);
            if self.at(&Tok::Question) {
                self.bump(); // nullable function type `((Int) -> Int)?`
            }
            // The annotation is a function type, NOT the type variable its
            // result may have mentioned — clear what the recursive call left.
            self.last_type_param = None;
            return Ok((Type::Unknown, Some("Function".to_string())));
        }
        let name = self.ident()?;
        if self.at(&Tok::Lt) {
            let mut depth = 0;
            loop {
                match self.bump() {
                    Tok::Lt => depth += 1,
                    Tok::Gt => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Tok::Eof => return Err("unterminated type argument list".into()),
                    _ => {}
                }
            }
        }
        let nullable = if self.at(&Tok::Question) {
            self.bump(); // nullable marker `T?`
            true
        } else {
            false
        };
        let ty = Type::from_name(&name);
        // `String?` is tracked apart from `String` so a null one still displays
        // as `null`; every other nullable annotation already displays through the
        // Kotlin stringifier, so the coarse type carries no other nullability.
        let ty = if nullable && ty == Type::String {
            Type::NullableString
        } else {
            ty
        };
        // A TYPE VARIABLE names no class at all. `Obj` would be a claim the
        // value is a heap object, which sends `+` down the operator-convention
        // dispatch (a `List`/`Map`/user-class `plus`) and fails at run time on
        // the `Int` or `String` a type argument actually supplied — so
        // `fun <T> id(x: T): T = x; id(1) + id(2)` reported `plus` unresolved
        // where the reference toolchain answers `3`. `Unknown` is the honest
        // width: it selects the value-directed ops and the runtime-tagged
        // display, and it is excluded from the 32-bit narrowing (see
        // `is_int_width`) precisely because the concrete type is not known.
        if self.type_params.contains(&name) {
            self.last_type_param = Some(name);
            return Ok((Type::Unknown, None));
        }
        self.last_type_param = None;
        // A non-primitive annotation names a heap object (a user class or a
        // container like `List`/`Map`); keep its name for dispatch.
        if ty == Type::Unknown {
            Ok((Type::Obj, Some(name)))
        } else {
            Ok((ty, None))
        }
    }

    fn block(&mut self) -> Result<Vec<Stmt>, String> {
        self.eat(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
            if self.at(&Tok::Semi) {
                self.bump();
                continue;
            }
            stmts.push(self.stmt()?);
        }
        self.eat(&Tok::RBrace)?;
        Ok(stmts)
    }

    // ── Statements ─────────────────────────────────────────────────

    fn stmt(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        // A loop label: `outer@ for (…)` / `outer@ while (…)`.
        if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::At) {
            let label = self.ident()?;
            self.eat(&Tok::At)?;
            let kind = match self.peek() {
                Tok::While => self.while_stmt(Some(label))?,
                Tok::Do => self.do_while_stmt(Some(label))?,
                Tok::For => self.for_stmt(Some(label))?,
                other => {
                    return Err(format!(
                        "a label must precede a loop (`for`/`while`/`do`), found {other:?}"
                    ))
                }
            };
            return Ok(Stmt::new(line, kind));
        }
        let kind = match self.peek() {
            Tok::Val | Tok::Var => self.let_decl()?,
            // A local `fun`, declared inside another function's body.
            Tok::Fun => StmtKind::LocalFun(self.fun_decl()?),
            Tok::Return => {
                self.bump();
                // `return@label` — a LOCAL return from the lambda (or `fun`)
                // carrying that label. Every lambda body here compiles to its
                // own VM frame, so a local return IS a frame return and the
                // label needs no lowering of its own; it is consumed and
                // dropped. (Kotlin's non-local `return` from an inline
                // function's lambda is a different construct and is unaffected.)
                if self.at(&Tok::At) {
                    self.bump();
                    self.ident()?;
                }
                // A `return` with no expression (Unit) — the next token starts a
                // new statement or closes the block.
                if matches!(self.peek(), Tok::RBrace | Tok::Semi | Tok::Eof) {
                    StmtKind::Return(None)
                } else {
                    StmtKind::Return(Some(self.expr()?))
                }
            }
            Tok::While => self.while_stmt(None)?,
            Tok::Do => self.do_while_stmt(None)?,
            Tok::For => self.for_stmt(None)?,
            Tok::If => StmtKind::If(self.if_expr()?),
            Tok::When => StmtKind::When(self.when_expr()?),
            Tok::Break => {
                self.bump();
                StmtKind::Break(self.opt_label()?)
            }
            Tok::Continue => {
                self.bump();
                StmtKind::Continue(self.opt_label()?)
            }
            _ => self.assign_or_expr()?,
        };
        Ok(Stmt::new(line, kind))
    }

    /// An optional `@label` after `break`/`continue`.
    fn opt_label(&mut self) -> Result<Option<String>, String> {
        if self.at(&Tok::At) {
            self.bump();
            Ok(Some(self.ident()?))
        } else {
            Ok(None)
        }
    }

    fn let_decl(&mut self) -> Result<StmtKind, String> {
        let mutable = matches!(self.bump(), Tok::Var);
        // Destructuring: `val (a, b, …) = expr`.
        if self.at(&Tok::LParen) {
            self.bump();
            let mut names = Vec::new();
            while !self.at(&Tok::RParen) {
                names.push(self.ident()?);
                if self.at(&Tok::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
            self.eat(&Tok::RParen)?;
            self.eat(&Tok::Assign)?;
            let init = self.expr()?;
            return Ok(StmtKind::Destructure { names, init });
        }
        let name = self.ident()?;
        let (ty, fn_params, fn_ret) = if self.at(&Tok::Colon) {
            self.bump();
            let before = self.fn_param_types.len();
            let ret_before = self.fn_ret_types.len();
            let t = self.type_name()?;
            (
                Some(t),
                self.fn_param_types.split_off(before),
                self.fn_ret_types.split_off(ret_before).pop(),
            )
        } else {
            (None, Vec::new(), None)
        };
        self.eat(&Tok::Assign)?;
        let init = self.expr()?;
        Ok(StmtKind::Let {
            name,
            ty,
            fn_params,
            fn_ret,
            init,
            mutable,
        })
    }

    fn while_stmt(&mut self, label: Option<String>) -> Result<StmtKind, String> {
        self.eat(&Tok::While)?;
        self.eat(&Tok::LParen)?;
        let cond = self.expr()?;
        self.eat(&Tok::RParen)?;
        let body = self.loop_body()?;
        Ok(StmtKind::While { cond, body, label })
    }

    /// `do { … } while (cond)`. The body is a block or a single statement, the
    /// same two forms `while` accepts.
    fn do_while_stmt(&mut self, label: Option<String>) -> Result<StmtKind, String> {
        self.eat(&Tok::Do)?;
        let body = self.loop_body()?;
        self.eat(&Tok::While)?;
        self.eat(&Tok::LParen)?;
        let cond = self.expr()?;
        self.eat(&Tok::RParen)?;
        Ok(StmtKind::DoWhile { cond, body, label })
    }

    /// A loop body: a `{ … }` block or, as Kotlin also allows, a single
    /// statement (`for (i in 1..3) println(i)`).
    fn loop_body(&mut self) -> Result<Vec<Stmt>, String> {
        if self.at(&Tok::LBrace) {
            self.block()
        } else {
            Ok(vec![self.stmt()?])
        }
    }

    /// `for (v in iterable) { … }`. The header is parsed as an ordinary
    /// expression, then split two ways: a *syntactic* range (`a..b`, `a until b`,
    /// `a downTo b`, each optionally `step n`) keeps the counted lowering, which
    /// runs on native fusevm ops; anything else (a `List`, an array, a range held
    /// in a variable) becomes a [`StmtKind::ForIn`] driven by host indexing.
    fn for_stmt(&mut self, label: Option<String>) -> Result<StmtKind, String> {
        self.eat(&Tok::For)?;
        self.eat(&Tok::LParen)?;
        // `for ((k, v) in map)` destructures each element; the plain form binds
        // it whole. The synthetic holder name carries a `$` so it can never
        // collide with a name the loop body writes.
        let mut parts = Vec::new();
        let var = if self.at(&Tok::LParen) {
            self.bump();
            while !self.at(&Tok::RParen) {
                parts.push(self.ident()?);
                if self.at(&Tok::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
            self.eat(&Tok::RParen)?;
            "$elem".to_string()
        } else {
            self.ident()?
        };
        self.eat(&Tok::In)?;
        let iter = self.expr()?;
        self.eat(&Tok::RParen)?;
        let body = self.loop_body()?;
        // Peel an optional `step n` off a literal range header.
        let (range, step) = match iter {
            Expr::Step { recv, by } => (*recv, Some(*by)),
            other => (other, None),
        };
        match range {
            // A destructuring header always takes the indexed form: the counted
            // range lowering binds one integer, which has no components.
            Expr::Range { start, end, kind } if parts.is_empty() => Ok(StmtKind::For {
                var,
                start: *start,
                end: *end,
                kind,
                step,
                body,
                label,
            }),
            // `step` on a non-range header has no counted form; rebuild the value
            // expression and iterate it.
            other => {
                let iter = match step {
                    Some(by) => Expr::Step {
                        recv: Box::new(other),
                        by: Box::new(by),
                    },
                    None => other,
                };
                Ok(StmtKind::ForIn {
                    var,
                    parts,
                    iter,
                    body,
                    label,
                })
            }
        }
    }

    /// A range endpoint — additive precedence, so `1..n-1` parses `n-1` as the
    /// end without swallowing the `..`.
    fn range_bound(&mut self) -> Result<Expr, String> {
        self.additive()
    }

    fn assign_or_expr(&mut self) -> Result<StmtKind, String> {
        // Parse a (potential) lvalue expression, then look for an assignment
        // operator. This uniformly covers `x = …`, `obj.field = …`, and
        // `coll[i] = …` (plus their `op=` forms) without special-casing.
        // `x++` / `++x` are consumed by the expression grammar (see
        // [`Parser::postfix`]), so a bare increment arrives here as an `IncDec`
        // expression statement whose value the compiler discards.
        let lhs = self.expr()?;

        let op = match self.peek() {
            Tok::Assign => Some(None),
            Tok::PlusEq => Some(Some(BinOp::Add)),
            Tok::MinusEq => Some(Some(BinOp::Sub)),
            Tok::StarEq => Some(Some(BinOp::Mul)),
            Tok::SlashEq => Some(Some(BinOp::Div)),
            Tok::PercentEq => Some(Some(BinOp::Mod)),
            _ => None,
        };
        let Some(binop) = op else {
            return Ok(StmtKind::Expr(lhs));
        };
        self.bump(); // the assign token
        let value = self.expr()?;
        self.assign_to(lhs, binop, value)
    }

    /// Build the assignment statement for an already-parsed lvalue, shared by
    /// the `=`/`op=` forms and the `++`/`--` desugar.
    fn assign_to(
        &mut self,
        lhs: Expr,
        binop: Option<BinOp>,
        value: Expr,
    ) -> Result<StmtKind, String> {
        match lhs {
            Expr::Var(name) => Ok(StmtKind::Assign {
                name,
                op: binop,
                value,
            }),
            Expr::Member {
                recv,
                name,
                safe: false,
                ..
            } => Ok(StmtKind::SetMember {
                recv: *recv,
                name,
                op: binop,
                value,
            }),
            Expr::Index { recv, index, .. } => Ok(StmtKind::SetIndex {
                recv: *recv,
                index: *index,
                op: binop,
                value,
            }),
            _ => Err(format!("invalid assignment target (line {})", self.line())),
        }
    }

    // ── Expressions ────────────────────────────────────────────────

    pub fn expr(&mut self) -> Result<Expr, String> {
        self.or_expr()
    }

    fn or_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.and_expr()?;
        while self.at(&Tok::OrOr) {
            self.bump();
            let r = self.and_expr()?;
            l = Expr::Binary {
                op: BinOp::Or,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    fn and_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.eq_expr()?;
        while self.at(&Tok::AndAnd) {
            self.bump();
            let r = self.eq_expr()?;
            l = Expr::Binary {
                op: BinOp::And,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    fn eq_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.cmp_expr()?;
        loop {
            let op = match self.peek() {
                Tok::EqEq => BinOp::Eq,
                Tok::NotEq => BinOp::Ne,
                _ => break,
            };
            self.bump();
            let r = self.cmp_expr()?;
            l = Expr::Binary {
                op,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    fn cmp_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.in_expr()?;
        loop {
            let op = match self.peek() {
                Tok::Lt => BinOp::Lt,
                Tok::Gt => BinOp::Gt,
                Tok::Le => BinOp::Le,
                Tok::Ge => BinOp::Ge,
                _ => break,
            };
            self.bump();
            let r = self.in_expr()?;
            l = Expr::Binary {
                op,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    /// Membership `a in b` / `a !in b`. Kotlin puts these at their own precedence
    /// level, looser than `?:` and tighter than the comparisons, so
    /// `x in 1..5` groups as `x in (1..5)` and `a in b == c` as `(a in b) == c`.
    /// A `for` header consumes its own `in` before reaching an expression, so the
    /// two uses never collide.
    /// Whether the parser sits on a `when` arm's `is Type ->` / `!is Type ->`
    /// header rather than on an `is` operator continuing the current expression.
    fn at_when_is_arm(&self) -> bool {
        let off = usize::from(self.at(&Tok::Not));
        if !matches!(self.peek_at(off), Tok::Is) || !matches!(self.peek_at(off + 1), Tok::Ident(_))
        {
            return false;
        }
        // Step over the type's own decorations (`is List<*> ->`, `is String? ->`)
        // so the `->` that marks an arm is still found behind them.
        let mut i = off + 2;
        if matches!(self.peek_at(i), Tok::Lt) {
            let mut depth = 0;
            loop {
                match self.peek_at(i) {
                    Tok::Lt => depth += 1,
                    Tok::Gt => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    Tok::Eof => return false,
                    _ => {}
                }
                i += 1;
            }
        }
        if matches!(self.peek_at(i), Tok::Question) {
            i += 1;
        }
        matches!(self.peek_at(i), Tok::Arrow)
    }

    fn in_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.elvis_expr()?;
        loop {
            // `is Type` / `!is Type` share this precedence level with `in`
            // (Kotlin's "named checks"), so `x is Dog == true` groups as
            // `(x is Dog) == true`.
            //
            // The lexer drops newlines, so a `when` arm body that ends in an
            // expression sits directly against the NEXT arm's `is Type ->`. An
            // `is` whose type is immediately followed by `->` therefore opens an
            // arm and is left for the `when` parser: `x is T ->` is never a
            // valid expression on its own (`->` only closes a lambda's
            // parameter list), so the test cannot misread real code.
            if self.at_when_is_arm() {
                break;
            }
            if self.at(&Tok::Is) || (self.at(&Tok::Not) && matches!(self.peek_at(1), Tok::Is)) {
                let negated = self.at(&Tok::Not);
                if negated {
                    self.bump();
                }
                self.bump(); // is
                let ty = self.is_type()?;
                l = Expr::Is {
                    value: Box::new(l),
                    ty,
                    negated,
                };
                continue;
            }
            let negated = if self.at(&Tok::In) {
                self.bump();
                false
            } else if self.at(&Tok::Not) && matches!(self.peek_at(1), Tok::In) {
                self.bump();
                self.bump();
                true
            } else {
                break;
            };
            let r = self.elvis_expr()?;
            l = Expr::In {
                value: Box::new(l),
                container: Box::new(r),
                negated,
            };
        }
        Ok(l)
    }

    /// Elvis `a ?: b`, right-associative, binding tighter than comparison and
    /// looser than additive (matching Kotlin, which places `?:` above named
    /// checks and comparisons). `?:` is `Question` immediately followed by
    /// `Colon`; a `?` followed by `.` is a safe call and stays in `postfix`.
    fn elvis_expr(&mut self) -> Result<Expr, String> {
        let l = self.infix_expr()?;
        if self.at(&Tok::Question) && matches!(self.peek_at(1), Tok::Colon) {
            self.bump(); // ?
            self.bump(); // :
            let r = self.elvis_expr()?;
            Ok(Expr::Elvis {
                left: Box::new(l),
                right: Box::new(r),
            })
        } else {
            Ok(l)
        }
    }

    /// The named infix functions, left-associative and all at one precedence
    /// level (Kotlin's `infixFunctionCall`): `to` (a `Pair`), `until` / `downTo`
    /// (ranges), and `step` (re-stepping a range). They are ordinary functions in
    /// Kotlin, not operators, which is why `1..10 step 2` parses as
    /// `(1..10) step 2` — `..` is a tighter level.
    fn infix_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.range_expr()?;
        loop {
            let kind = match self.peek() {
                Tok::Ident(n) if n == "to" => None,
                // The bitwise operators are ordinary infix MEMBER functions in
                // Kotlin (`Int.and`, `Int.shl`, …), which is exactly how they
                // lower here — so they need no operator of their own, and they
                // sit at this precedence level with the other named infix
                // functions.
                Tok::Ident(n) if is_bitwise_infix(n) => {
                    let name = self.ident()?;
                    let r = self.range_expr()?;
                    l = Expr::MethodCall {
                        recv: Box::new(l),
                        name,
                        args: vec![r],
                        safe: false,
                        line: self.line(),
                    };
                    continue;
                }
                Tok::Until => Some(RangeKind::Until),
                Tok::DownTo => Some(RangeKind::DownTo),
                Tok::Step => {
                    self.bump();
                    let by = self.range_expr()?;
                    l = Expr::Step {
                        recv: Box::new(l),
                        by: Box::new(by),
                    };
                    continue;
                }
                _ => break,
            };
            self.bump();
            let r = self.range_expr()?;
            l = match kind {
                None => Expr::Pair {
                    first: Box::new(l),
                    second: Box::new(r),
                },
                Some(k) => Expr::Range {
                    start: Box::new(l),
                    end: Box::new(r),
                    kind: k,
                },
            };
        }
        Ok(l)
    }

    /// The range operator `a..b`, binding tighter than the named infix functions
    /// and looser than `+`/`-` — so `1..n-1` is `1..(n-1)`, matching Kotlin.
    fn range_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.additive()?;
        while self.at(&Tok::DotDot) {
            self.bump();
            let r = self.additive()?;
            l = Expr::Range {
                start: Box::new(l),
                end: Box::new(r),
                kind: RangeKind::Inclusive,
            };
        }
        Ok(l)
    }

    fn additive(&mut self) -> Result<Expr, String> {
        let mut l = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let r = self.multiplicative()?;
            l = Expr::Binary {
                op,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    fn multiplicative(&mut self) -> Result<Expr, String> {
        let mut l = self.as_expr()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Mod,
                _ => break,
            };
            self.bump();
            let r = self.as_expr()?;
            l = Expr::Binary {
                op,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    /// `value as Type` / `value as? Type`. Kotlin binds the cast tighter than
    /// every binary operator, so `a as Int * 2` is `(a as Int) * 2` — hence its
    /// own level directly above `unary`.
    fn as_expr(&mut self) -> Result<Expr, String> {
        let mut e = self.unary()?;
        while matches!(self.peek(), Tok::Ident(w) if w == "as") {
            self.bump();
            let safe = self.at(&Tok::Question);
            if safe {
                self.bump();
            }
            let ty = self.is_type()?;
            e = Expr::As {
                value: Box::new(e),
                ty,
                safe,
            };
        }
        Ok(e)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Tok::Minus => {
                self.bump();
                Ok(Expr::Unary {
                    op: UnOp::Neg,
                    expr: Box::new(self.unary()?),
                })
            }
            Tok::Not => {
                self.bump();
                Ok(Expr::Unary {
                    op: UnOp::Not,
                    expr: Box::new(self.unary()?),
                })
            }
            // Prefix `++x` / `--x` — the value is the target AFTER the update.
            Tok::PlusPlus | Tok::MinusMinus => {
                let inc = matches!(self.peek(), Tok::PlusPlus);
                self.bump();
                Ok(Expr::IncDec {
                    target: Box::new(self.unary()?),
                    inc,
                    prefix: true,
                })
            }
            _ => self.postfix(),
        }
    }

    /// Postfix `.` chains: `recv.property` and `recv.method(args)`, left-
    /// associative and chainable (`a.b.c()`). Binds tighter than the prefix
    /// unary operators, so `-a.b` is `-(a.b)`, matching Kotlin.
    fn postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.primary()?;
        loop {
            // Not-null assertion `expr!!` — two consecutive `!` tokens (`!=`
            // lexes as a single `NotEq`, so this only fires on a literal `!!`).
            if self.at(&Tok::Not) && matches!(self.peek_at(1), Tok::Not) {
                self.bump();
                self.bump();
                e = Expr::NotNull(Box::new(e));
                continue;
            }
            // Invocation of the value the chain has produced so far — `f()()`,
            // `lst[0](7)`, `{ x: Int -> x }(9)`, `m["k"]!!()`. Restricted to
            // chains that already ended in a call/index/lambda so a bare value
            // is never swallowed, and required to be glued to the previous
            // token so a statement-leading `(1..3).forEach { … }` after
            // `f()` stays a separate statement, exactly as Kotlin parses it.
            if self.at(&Tok::LParen) && self.glued_to_prev() && invocable(&e) {
                let line = self.line();
                self.bump();
                let mut args = Vec::new();
                while !self.at(&Tok::RParen) {
                    args.push(self.call_arg()?);
                    if self.at(&Tok::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::RParen)?;
                if self.at(&Tok::LBrace) && !self.no_trailing_lambda {
                    args.push(self.lambda()?);
                }
                e = Expr::Invoke {
                    target: Box::new(e),
                    args,
                    line,
                };
                continue;
            }
            // Indexed access `recv[index]` (chainable: `m[k][i]`).
            if self.at(&Tok::LBracket) {
                let line = self.line();
                self.bump();
                let index = self.expr()?;
                self.eat(&Tok::RBracket)?;
                e = Expr::Index {
                    recv: Box::new(e),
                    index: Box::new(index),
                    line,
                };
                continue;
            }
            // Plain member/method `.` or safe-call `?.`.
            let safe = if self.at(&Tok::Dot) {
                false
            } else if self.at(&Tok::Question) && matches!(self.peek_at(1), Tok::Dot) {
                self.bump(); // `?`
                true
            } else {
                break;
            };
            let line = self.line();
            self.bump(); // `.`
            let name = self.ident()?;
            let mut args = Vec::new();
            let mut is_call = false;
            if self.at(&Tok::LParen) {
                is_call = true;
                self.bump();
                while !self.at(&Tok::RParen) {
                    args.push(self.call_arg()?);
                    if self.at(&Tok::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::RParen)?;
            }
            // Trailing-lambda syntax: `list.map { … }` / `list.map(sel) { … }`.
            if self.at(&Tok::LBrace) && !self.no_trailing_lambda {
                is_call = true;
                args.push(self.lambda()?);
            }
            if is_call {
                e = Expr::MethodCall {
                    recv: Box::new(e),
                    name,
                    args,
                    safe,
                    line,
                };
            } else {
                e = Expr::Member {
                    recv: Box::new(e),
                    name,
                    safe,
                    line,
                };
            }
        }
        // Postfix `x++` / `x--` — the value is the target BEFORE the update.
        // Parsed here rather than desugared at statement level so it works in
        // expression position (`println(i++)`) as well as as a statement.
        if matches!(self.peek(), Tok::PlusPlus | Tok::MinusMinus) {
            let inc = matches!(self.peek(), Tok::PlusPlus);
            self.bump();
            e = Expr::IncDec {
                target: Box::new(e),
                inc,
                prefix: false,
            };
        }
        Ok(e)
    }

    /// A lambda literal `{ (p1, p2, …) -> body }` or `{ body }` (implicit `it`).
    /// The body is a statement sequence whose last statement's value is the
    /// lambda's result.
    fn lambda(&mut self) -> Result<Expr, String> {
        self.eat(&Tok::LBrace)?;
        // Optional parameter list ending in `->`. Speculatively scan a run of
        // `IDENT (',' IDENT)* '->'`; roll back if it isn't there.
        let mut params: Vec<(String, Type)> = Vec::new();
        let save = self.pos;
        if matches!(self.peek(), Tok::Ident(_)) {
            let mut tmp: Vec<(String, Type)> = Vec::new();
            let mut ok = true;
            loop {
                let pname = match self.peek() {
                    Tok::Ident(n) => {
                        let n = n.clone();
                        self.bump();
                        n
                    }
                    _ => {
                        ok = false;
                        break;
                    }
                };
                // Optional per-parameter type annotation `p: Type`. A simple named
                // type is captured (so `Int`/`Long` params drive integer op
                // selection in the body); a complex/function type is consumed and
                // left `Unknown`. On a run-in to a block terminator, roll back —
                // the `{ … }` was a body, not a parameter list.
                let mut pty = Type::Unknown;
                if self.at(&Tok::Colon) {
                    self.bump();
                    match self.skip_lambda_param_type() {
                        Some(t) => pty = t,
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                tmp.push((pname, pty));
                if self.at(&Tok::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
            if ok && self.at(&Tok::Arrow) {
                self.bump();
                params = tmp;
            } else {
                self.pos = save;
            }
        } else if self.at(&Tok::Arrow) {
            self.bump(); // `{ -> … }` — explicitly no parameters
        }
        let mut body = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
            if self.at(&Tok::Semi) {
                self.bump();
                continue;
            }
            body.push(self.stmt()?);
        }
        self.eat(&Tok::RBrace)?;
        Ok(Expr::Lambda { params, body })
    }

    /// Consume the tokens of a lambda parameter's type annotation, stopping
    /// before the next top-level `,` or `->`. The coarse type of a simple named
    /// annotation (`Int`, `String`, …) is returned so the body can pick integer
    /// vs float ops; a complex/function/generic type yields `Unknown`. Nested
    /// `<…>`/`(…)`/`[…]` are balanced so a generic type is skipped whole.
    /// Returns `None` on a run-in to a block terminator — the speculative param
    /// scan then rolls back (the `{ … }` was a body, not a parameter list).
    fn skip_lambda_param_type(&mut self) -> Option<Type> {
        // A lone leading identifier at depth 0 is the coarse type name; anything
        // more (generics, nullable, function type) widens it to `Unknown`.
        let mut ty = match self.peek() {
            Tok::Ident(n) => Type::from_name(n),
            _ => Type::Unknown,
        };
        let mut consumed_simple = false;
        let mut depth = 0i32;
        loop {
            match self.peek() {
                Tok::Comma | Tok::Arrow if depth == 0 => return Some(ty),
                Tok::Lt | Tok::LParen | Tok::LBracket => {
                    depth += 1;
                    ty = Type::Unknown;
                    self.bump();
                }
                Tok::Gt | Tok::RParen | Tok::RBracket => {
                    depth -= 1;
                    self.bump();
                }
                Tok::RBrace | Tok::Eof => return None,
                Tok::Ident(_) if depth == 0 && !consumed_simple => {
                    consumed_simple = true;
                    self.bump();
                }
                _ => {
                    // A second token at depth 0 (`Int?`, `a.B`, …) is not a plain
                    // simple type — fall back to `Unknown`.
                    ty = Type::Unknown;
                    self.bump();
                }
            }
        }
    }

    /// Consume `<T, U>` after a call name when what follows really is a type
    /// argument list — a balanced run of names (with `.`/`?`/nested `<…>`) ending
    /// in `>` immediately followed by `(`. Anything else rolls back, so
    /// `a < b` stays a comparison. Kotlin resolves the same ambiguity the same
    /// way: `a<b>(c)` is a generic call.
    fn skip_call_type_args(&mut self) {
        if !self.at(&Tok::Lt) {
            return;
        }
        let save = self.pos;
        self.bump(); // `<`
        let mut depth = 1i32;
        // A function type argument (`listOf<(Int) -> Int>()`) puts parens and an
        // `->` inside the list. They are tracked separately so the `>` that ends
        // a nested `Map<K, V>` is not confused with one inside `(…)`.
        let mut paren = 0i32;
        loop {
            match self.peek() {
                Tok::LParen => {
                    paren += 1;
                    self.bump();
                }
                Tok::RParen => {
                    paren -= 1;
                    if paren < 0 {
                        self.pos = save;
                        return;
                    }
                    self.bump();
                }
                Tok::Arrow => {
                    self.bump();
                }
                Tok::Lt => {
                    depth += 1;
                    self.bump();
                }
                Tok::Gt if paren == 0 => {
                    depth -= 1;
                    self.bump();
                    if depth == 0 {
                        break;
                    }
                }
                Tok::Ident(_) | Tok::Comma | Tok::Dot | Tok::Question => {
                    self.bump();
                }
                // Not a type-argument list after all.
                _ => {
                    self.pos = save;
                    return;
                }
            }
        }
        // A trailing lambda counts as the argument list: `buildList<Int> { … }`
        // has no parentheses at all, and Kotlin still reads it as a generic
        // call. Nothing else follows a real type-argument list, so requiring
        // one of the two is what keeps `a < b` a comparison.
        if !self.at(&Tok::LParen) && !self.at(&Tok::LBrace) {
            self.pos = save;
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        let line = self.line();
        match self.peek().clone() {
            Tok::Int(n) => {
                self.bump();
                Ok(Expr::Int(n))
            }
            Tok::Long(n) => {
                self.bump();
                Ok(Expr::Long(n))
            }
            Tok::Float(f) => {
                self.bump();
                Ok(Expr::Float(f))
            }
            Tok::Bool(b) => {
                self.bump();
                Ok(Expr::Bool(b))
            }
            Tok::Char(c) => {
                self.bump();
                Ok(Expr::Char(c))
            }
            Tok::Null => {
                self.bump();
                Ok(Expr::Null)
            }
            Tok::Str(parts) => {
                self.bump();
                Ok(Expr::Str(self.str_parts(&parts)?))
            }
            Tok::If => Ok(Expr::If(self.if_expr()?)),
            Tok::When => Ok(Expr::When(self.when_expr()?)),
            Tok::Try => Ok(Expr::Try(self.try_expr()?)),
            // `throw e` — an expression (Kotlin types it `Nothing`), so it is
            // usable as a statement and on the right of `?:`.
            Tok::Throw => {
                self.bump();
                Ok(Expr::Throw(Box::new(self.expr()?)))
            }
            // A brace in expression position is a lambda literal (`val f = { … }`,
            // `f({ … })`). Trailing-lambda braces are consumed by `postfix` /
            // `primary`'s call arms before reaching here.
            Tok::LBrace => self.lambda(),
            Tok::LParen => {
                self.bump();
                let e = self.expr()?;
                self.eat(&Tok::RParen)?;
                Ok(e)
            }
            // A soft keyword in leading expression position is a plain name: the
            // infix forms (`a until b`, `r step 2`) consume theirs from the
            // operator loop, which has a left operand and never reaches here,
            // and a `data class` declaration is taken by the top-level parser.
            ref t if soft_keyword(t).is_some() => {
                let name = soft_keyword(t).unwrap().to_string();
                self.bump();
                if self.at(&Tok::LParen) {
                    self.primary_call(name, line)
                } else {
                    Ok(Expr::Var(name))
                }
            }
            Tok::Ident(name) if name == "super" => {
                self.bump();
                // `super<Base>.m()` — the supertype qualifier that disambiguates
                // when more than one supertype implements `m`. Not consumed by
                // `skip_call_type_args`, which only takes a type-argument list
                // followed by `(`; this one is followed by `.`.
                let qualifier = if self.at(&Tok::Lt) {
                    self.bump();
                    let n = self.ident()?;
                    self.eat(&Tok::Gt)?;
                    Some(n)
                } else {
                    None
                };
                Ok(Expr::Super { qualifier })
            }
            Tok::Ident(name) => {
                self.bump();
                // Explicit type arguments on a call — `listOf<Int>()`,
                // `emptyMap<String, Int>()`. Consumed and ignored (typing here is
                // coarse); the speculative scan below is what keeps `a < b` a
                // comparison, since a type-argument list may only hold names and
                // must be followed by `(`.
                self.skip_call_type_args();
                if self.at(&Tok::LParen) {
                    self.bump();
                    let mut args = Vec::new();
                    while !self.at(&Tok::RParen) {
                        args.push(self.call_arg()?);
                        if self.at(&Tok::Comma) {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    self.eat(&Tok::RParen)?;
                    // Trailing-lambda syntax on a free call: `apply(x) { … }`.
                    if self.at(&Tok::LBrace) && !self.no_trailing_lambda {
                        args.push(self.lambda()?);
                    }
                    Ok(Expr::Call { name, args, line })
                } else if self.at(&Tok::LBrace) && !self.no_trailing_lambda {
                    // Bare trailing-lambda call `run { … }` (no parenthesized
                    // args). `Ident {` is unambiguously a call in Kotlin's
                    // expression grammar — there are no anonymous block statements.
                    let lam = self.lambda()?;
                    Ok(Expr::Call {
                        name,
                        args: vec![lam],
                        line,
                    })
                } else {
                    Ok(Expr::Var(name))
                }
            }
            other => Err(format!("unexpected token {:?} (line {})", other, line)),
        }
    }

    /// One call argument: `name = value` (a named argument) or a plain
    /// expression. Kotlin has no assignment *expression*, so an identifier
    /// followed by `=` inside an argument list can only be the named form.
    fn call_arg(&mut self) -> Result<Expr, String> {
        if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Assign) {
            let name = self.ident()?;
            self.bump(); // `=`
            return Ok(Expr::Named {
                name,
                value: Box::new(self.expr()?),
            });
        }
        self.expr()
    }

    /// The argument list of a parenthesized call whose callee name is already
    /// consumed, plus an optional trailing lambda.
    fn primary_call(&mut self, name: String, line: u32) -> Result<Expr, String> {
        self.eat(&Tok::LParen)?;
        let mut args = Vec::new();
        while !self.at(&Tok::RParen) {
            args.push(self.call_arg()?);
            if self.at(&Tok::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.eat(&Tok::RParen)?;
        if self.at(&Tok::LBrace) {
            args.push(self.lambda()?);
        }
        Ok(Expr::Call { name, args, line })
    }

    fn if_expr(&mut self) -> Result<IfExpr, String> {
        let line = self.line();
        self.eat(&Tok::If)?;
        self.eat(&Tok::LParen)?;
        let cond = self.expr()?;
        self.eat(&Tok::RParen)?;
        let then = self.branch_body()?;
        let els = if self.at(&Tok::Else) {
            self.bump();
            if self.at(&Tok::If) {
                // `else if` chains as an else-branch holding a single if-stmt.
                let l = self.line();
                Some(vec![Stmt::new(l, StmtKind::If(self.if_expr()?))])
            } else {
                Some(self.branch_body()?)
            }
        } else {
            None
        };
        Ok(IfExpr {
            cond: Box::new(cond),
            then,
            els,
            line,
        })
    }

    /// `try { … } catch (e: T) { … }* [finally { … }]`. Kotlin requires braces on
    /// every part and at least one `catch` or a `finally`.
    fn try_expr(&mut self) -> Result<TryExpr, String> {
        let line = self.line();
        self.eat(&Tok::Try)?;
        let body = self.block()?;
        let mut catches = Vec::new();
        while self.at(&Tok::Catch) {
            self.bump();
            self.eat(&Tok::LParen)?;
            let name = self.ident()?;
            self.eat(&Tok::Colon)?;
            // The caught type is a plain name; a nullable/generic spelling is not
            // valid on a `catch` parameter in Kotlin, so no `type_ref` here.
            let ty = self.ident()?;
            self.eat(&Tok::RParen)?;
            let cbody = self.block()?;
            catches.push(CatchArm {
                name,
                ty,
                body: cbody,
            });
        }
        // An empty `finally { }` is still a `finally` for the purposes of the
        // “a try needs one or the other” rule, even though it lowers to nothing.
        let mut saw_finally = false;
        let finally_body = if self.at(&Tok::Finally) {
            self.bump();
            saw_finally = true;
            self.block()?
        } else {
            Vec::new()
        };
        if catches.is_empty() && !saw_finally {
            return Err(format!(
                "a `try` needs at least one `catch` or a `finally` (line {line})"
            ));
        }
        Ok(TryExpr {
            body,
            catches,
            finally_body,
            line,
        })
    }

    /// A `when` — subject form `when (x) { … }` or subjectless `when { … }`.
    /// Arms are `guard -> body`, with `guard` either `else`, or one or more
    /// comma-separated conditions.
    fn when_expr(&mut self) -> Result<WhenExpr, String> {
        let line = self.line();
        self.eat(&Tok::When)?;
        // `when (val n = subject)` names the subject for the arm bodies; the
        // plain form is the same thing without the name.
        let mut binding = None;
        let subject = if self.at(&Tok::LParen) {
            self.bump();
            if self.at(&Tok::Val) {
                self.bump();
                binding = Some(self.ident()?);
                if self.at(&Tok::Colon) {
                    self.bump();
                    self.type_name()?;
                }
                self.eat(&Tok::Assign)?;
            }
            let e = self.expr()?;
            self.eat(&Tok::RParen)?;
            Some(Box::new(e))
        } else {
            None
        };
        let has_subject = subject.is_some();
        self.eat(&Tok::LBrace)?;
        let mut arms = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
            if self.at(&Tok::Semi) {
                self.bump();
                continue;
            }
            let guard = if self.at(&Tok::Else) {
                self.bump();
                WhenGuard::Else
            } else {
                let mut conds = vec![self.when_cond(has_subject)?];
                while self.at(&Tok::Comma) {
                    self.bump();
                    conds.push(self.when_cond(has_subject)?);
                }
                WhenGuard::Conds(conds)
            };
            self.eat(&Tok::Arrow)?;
            let body = self.branch_body()?;
            arms.push(WhenArm { guard, body });
        }
        self.eat(&Tok::RBrace)?;
        Ok(WhenExpr {
            subject,
            binding,
            arms,
            line,
        })
    }

    /// A single `when` arm condition. In subject form it may be `in range`,
    /// `!in range`, `is Type`, `!is Type`, or an expression (equality against
    /// the subject). In subjectless form it is a boolean expression.
    fn when_cond(&mut self, has_subject: bool) -> Result<WhenCond, String> {
        if has_subject {
            match self.peek() {
                Tok::In => {
                    self.bump();
                    return self.when_range(false);
                }
                Tok::Is => {
                    self.bump();
                    let ty = self.is_type()?;
                    return Ok(WhenCond::Is { negated: false, ty });
                }
                // `!in` / `!is` — a `!` immediately followed by `in`/`is`.
                Tok::Not if matches!(self.peek_at(1), Tok::In) => {
                    self.bump();
                    self.bump();
                    return self.when_range(true);
                }
                Tok::Not if matches!(self.peek_at(1), Tok::Is) => {
                    self.bump();
                    self.bump();
                    let ty = self.is_type()?;
                    return Ok(WhenCond::Is { negated: true, ty });
                }
                _ => {}
            }
        }
        Ok(WhenCond::Expr(self.expr()?))
    }

    /// The type after `is`/`!is`: a name, optionally with type arguments
    /// (`List<*>`) and a nullable marker (`String?`). The check is by erased
    /// class, so both decorations are consumed and discarded — matching the JVM,
    /// where `is List<String>` can only test that the value is a `List`.
    fn is_type(&mut self) -> Result<String, String> {
        let ty = self.ident()?;
        self.skip_type_args();
        if self.at(&Tok::Question) {
            self.bump();
        }
        if self.type_params.contains(&ty) {
            return Err(format!(
                "a runtime test against the type parameter `{ty}` needs a reified \
                 type argument, which is not supported (line {})",
                self.line()
            ));
        }
        Ok(ty)
    }

    /// The range after `in`/`!in` in a `when` arm — `a..b`, `a until b`, or
    /// `a downTo b`.
    fn when_range(&mut self, negated: bool) -> Result<WhenCond, String> {
        let start = self.range_bound()?;
        let (kind, end) = match self.peek() {
            Tok::DotDot => {
                self.bump();
                (RangeKind::Inclusive, self.range_bound()?)
            }
            Tok::Until => {
                self.bump();
                (RangeKind::Until, self.range_bound()?)
            }
            Tok::DownTo => {
                self.bump();
                (RangeKind::DownTo, self.range_bound()?)
            }
            other => {
                return Err(format!(
                "`in` condition needs a range (`a..b`, `a until b`, `a downTo b`), found {other:?}"
            ))
            }
        };
        Ok(WhenCond::InRange {
            negated,
            start,
            end,
            kind,
        })
    }

    /// An `if`/`else`/`when` branch body: either a `{ … }` block or a single
    /// statement. The single form covers a value expression (`if (c) e1 else e2`)
    /// as well as the control-flow forms Kotlin permits there — `break`,
    /// `continue`, `return`, and a nested `when` — which are statements, not
    /// expressions.
    fn branch_body(&mut self) -> Result<Vec<Stmt>, String> {
        if self.at(&Tok::LBrace) {
            self.block()
        } else {
            Ok(vec![self.stmt()?])
        }
    }

    /// Turn lexed [`StrPart`]s into [`StrExpr`]s, sub-parsing each interpolation
    /// fragment as its own expression.
    fn str_parts(&self, parts: &[StrPart]) -> Result<Vec<StrExpr>, String> {
        let mut out = Vec::with_capacity(parts.len());
        for p in parts {
            match p {
                StrPart::Text(t) => out.push(StrExpr::Text(t.clone())),
                StrPart::Expr(src) => {
                    let toks = Lexer::new(src).tokenize()?;
                    let mut sub = Parser {
                        type_params: Vec::new(),
                        class_type_params: Vec::new(),
                        toks,
                        pos: 0,
                        fn_param_types: Vec::new(),
                        fn_ret_types: Vec::new(),
                        last_type_param: None,
                        no_trailing_lambda: false,
                        // A string template holds an expression, which can
                        // never declare a class.
                        pending_classes: Vec::new(),
                    };
                    let e = sub.expr()?;
                    out.push(StrExpr::Expr(Box::new(e)));
                }
            }
        }
        Ok(out)
    }
}
