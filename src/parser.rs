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

pub struct Parser {
    toks: Vec<Spanned>,
    pos: usize,
    /// Parameter types collected while scanning function-type annotations, in
    /// source order. [`Parser::type_ref`] appends; the `val`/`var` rule takes
    /// the slice its own annotation contributed. A side channel rather than a
    /// return value because a function type may nest inside a generic argument
    /// several `type_ref` frames down, where the caller has nowhere to put it.
    fn_param_types: Vec<Type>,
    /// The type-parameter names of the `fun` whose body is being parsed
    /// (`fun <T> f(…)`). A runtime test against one of them — `x is T`, `x as T`
    /// — needs a `reified` type argument the coarse type system cannot carry, so
    /// it is rejected here rather than silently answering for a class named `T`
    /// that does not exist.
    type_params: Vec<String>,
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
        type_params: Vec::new(),
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
    // Hoist each `companion object` to the top level. From here on it is an
    // ordinary singleton, and only the owner→companion NAME relation (which
    // `companion_name` reconstructs) is needed to resolve `Owner.member`.
    let hoisted: Vec<ClassDecl> = prog
        .classes
        .iter_mut()
        .filter_map(|cd| cd.companion.take().map(|c| *c))
        .collect();
    prog.classes.extend(hoisted);
    Ok(prog)
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
        let tps = self.type_params_decl();
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
                (Type::Unknown, None)
            };
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
        let (ret_annot, ret_class) = if self.at(&Tok::Colon) {
            self.bump();
            let (t, c) = self.type_ref()?;
            (Some(t), c)
        } else {
            (None, None)
        };
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
        // carries no type variables.
        self.skip_type_args();

        // Primary constructor (classes only). `object`s and `interface`s have
        // none.
        let mut params = Vec::new();
        if !is_object && !is_interface && self.at(&Tok::LParen) {
            self.bump();
            while !self.at(&Tok::RParen) {
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
                });
                if self.at(&Tok::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
            self.eat(&Tok::RParen)?;
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
        if self.at(&Tok::LBrace) {
            self.bump();
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
                let mods = self.modifiers();
                match self.peek() {
                    Tok::Fun => methods.push(self.fun_decl_mods(mods)?),
                    // A body property: `val n = expr` / `var c: Int = expr`, in a
                    // class as well as an `object`. An `interface` has no
                    // storage to put one in, so it is rejected there.
                    Tok::Val | Tok::Var => {
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

        Ok(ClassDecl {
            name,
            params,
            obj_props,
            methods,
            is_data,
            is_object,
            is_interface,
            is_abstract: mods.abstract_,
            is_open: mods.open,
            is_sealed: mods.sealed,
            parents,
            super_args,
            companion,
            line,
        })
    }

    /// A stored property with an initializer: `val n: Int = 5`, `var c = 0`, or
    /// the delegated form `val z: Int by lazy { … }`. Used for a class body, an
    /// `object` body, and the top level alike — the three differ in WHERE the
    /// initializer runs, not in how it is written.
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
            let what = self.ident()?;
            if what != "lazy" {
                return Err(format!(
                    "property {name}: `by {what}` is not supported; only `by lazy` is"
                ));
            }
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
            let _ret = self.type_ref()?; // return type (may itself be a fn type)
            if self.at(&Tok::Question) {
                self.bump(); // nullable function type `((Int) -> Int)?`
            }
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
        let (ty, fn_params) = if self.at(&Tok::Colon) {
            self.bump();
            let before = self.fn_param_types.len();
            let t = self.type_name()?;
            (Some(t), self.fn_param_types.split_off(before))
        } else {
            (None, Vec::new())
        };
        self.eat(&Tok::Assign)?;
        let init = self.expr()?;
        Ok(StmtKind::Let {
            name,
            ty,
            fn_params,
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
            if self.at(&Tok::LBrace) {
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
        loop {
            match self.peek() {
                Tok::Lt => {
                    depth += 1;
                    self.bump();
                }
                Tok::Gt => {
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
        if !self.at(&Tok::LParen) {
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
                    if self.at(&Tok::LBrace) {
                        args.push(self.lambda()?);
                    }
                    Ok(Expr::Call { name, args, line })
                } else if self.at(&Tok::LBrace) {
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
                        toks,
                        pos: 0,
                        fn_param_types: Vec::new(),
                    };
                    let e = sub.expr()?;
                    out.push(StrExpr::Expr(Box::new(e)));
                }
            }
        }
        Ok(out)
    }
}
