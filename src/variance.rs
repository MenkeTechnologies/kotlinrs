//! Declaration-site and use-site variance, checked the way `kotlinc` checks
//! them, over the written types the parser keeps beside the coarse ones (see
//! [`TypeExpr`]).
//!
//! Every check here REJECTS programs, so each one only speaks where the answer
//! is certain: a generic class whose parameters' variance is not known (a JDK
//! class this table does not list, an unresolved name) contributes nothing, and
//! neither does a type the parser could not model. A missed rejection is a
//! program accepted that `kotlinc` refuses; a false one would refuse a valid
//! program, which is the failure these rules are built to never commit.

use crate::ast::*;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use Variance::{In, Invariant as Inv, Out};

/// The declared variance of a standard-library generic's parameters, by its
/// simple name. Measured against `kotlinc` 2.4.21: each entry's parameters
/// were placed in an `out` class's result and parameter positions and the
/// reported position read back.
fn stdlib_variance(head: &str) -> Option<&'static [Variance]> {
    Some(match head {
        "List" | "Collection" | "Iterable" | "Set" | "Sequence" | "Iterator" | "ListIterator"
        | "Lazy" | "Result" | "IndexedValue" | "MutableIterable" | "MutableIterator" => &[Out],
        "Pair" | "Entry" => &[Out, Out],
        "Triple" => &[Out, Out, Out],
        "Map" => &[Inv, Out],
        "Comparable" => &[In],
        "Comparator"
        | "Array"
        | "MutableList"
        | "MutableSet"
        | "MutableCollection"
        | "ArrayList"
        | "HashSet"
        | "LinkedHashSet"
        | "ArrayDeque"
        | "MutableListIterator" => &[Inv],
        "MutableMap" | "HashMap" | "LinkedHashMap" | "Grouping" => &[Inv, Inv],
        _ => return None,
    })
}

/// The standard-library names that are `typealias`es on the JVM. A type
/// parameter whose nearest enclosing generic is one of these is reported "in
/// abbreviated type" rather than "in type".
fn is_jvm_typealias(head: &str) -> bool {
    matches!(
        head,
        "ArrayList" | "HashMap" | "HashSet" | "LinkedHashMap" | "LinkedHashSet" | "Comparator"
    )
}

/// What the use-site checks need of one user class.
pub struct ClassVariance {
    pub type_params: Vec<String>,
    pub variance: Vec<Variance>,
    pub members: Vec<MemberType>,
}

/// The declared variance of every generic class in scope: the program's own,
/// by hoisted name and by simple name, falling back to the standard library.
#[derive(Default)]
pub struct VarianceTable {
    /// `None` marks a simple name two classes share, which resolves to neither.
    user: HashMap<String, Option<Rc<ClassVariance>>>,
    /// Top-level function names: a call `F<A>(…)` to one constructs nothing.
    funs: HashSet<String>,
}

impl VarianceTable {
    pub fn new(program: &Program) -> Self {
        let mut user: HashMap<String, Option<Rc<ClassVariance>>> = HashMap::new();
        for c in &program.classes {
            let info = Rc::new(ClassVariance {
                type_params: c.type_params.clone(),
                variance: c.variance.clone(),
                members: c.member_types.clone(),
            });
            let simple = c.name.rsplit('$').next().unwrap_or(&c.name).to_string();
            let mut names = vec![c.name.clone()];
            if simple != c.name {
                names.push(simple);
            }
            for n in names {
                user.entry(n)
                    .and_modify(|v| *v = None)
                    .or_insert(Some(info.clone()));
            }
        }
        let funs = program.funs.iter().map(|f| f.name.clone()).collect();
        VarianceTable { user, funs }
    }

    /// The user class a written type names, when exactly one does.
    fn user_class(&self, ty: &TypeExpr) -> Option<&Rc<ClassVariance>> {
        let TypeExpr::Named { name, .. } = ty else {
            return None;
        };
        self.user.get(&name.replace('.', "$"))?.as_ref()
    }

    /// Whether the written name resolves to some user class at all (even an
    /// ambiguous one), which then shadows the standard library's.
    fn names_user_class(&self, ty: &TypeExpr) -> bool {
        matches!(ty, TypeExpr::Named { name, .. } if self.user.contains_key(&name.replace('.', "$")))
    }

    /// The parameters' variance for a written generic, when it is known and
    /// agrees with the number of arguments written.
    pub fn of(&self, ty: &TypeExpr) -> Option<&[Variance]> {
        let TypeExpr::Named { args, .. } = ty else {
            return None;
        };
        let vs: &[Variance] = if self.names_user_class(ty) {
            &self.user_class(ty)?.variance
        } else {
            stdlib_variance(ty.head()?)?
        };
        (vs.len() == args.len()).then_some(vs)
    }
}

/// Type parameters in scope, each with its declared variance.
type Params = Vec<(String, Variance)>;

/// One violation: the type parameter, its declared variance, the position it
/// occurs in, and whether its nearest generic is a JVM typealias.
struct Violation {
    name: String,
    declared: Variance,
    pos: Variance,
    abbreviated: bool,
}

/// Walk `ty` in position `pos`, answering the first occurrence of one of
/// `params` (with its declared variance) that the position forbids.
fn first_violation(
    table: &VarianceTable,
    ty: &TypeExpr,
    pos: Variance,
    params: &[(String, Variance)],
    in_alias: bool,
) -> Option<Violation> {
    match ty {
        TypeExpr::Named { name, args, .. } if args.is_empty() => {
            let (_, declared) = params.iter().find(|(n, _)| n == name)?;
            let bad = match declared {
                Out => pos != Out,
                In => pos != In,
                Inv => false,
            };
            bad.then(|| Violation {
                name: name.clone(),
                declared: *declared,
                pos,
                abbreviated: in_alias,
            })
        }
        TypeExpr::Named { args, .. } => {
            let vs = table.of(ty)?;
            let alias = ty.head().is_some_and(is_jvm_typealias);
            args.iter().zip(vs).find_map(|(a, declared)| match a {
                TypeProj::Star => None,
                TypeProj::Arg(proj, t) => {
                    let arg = match (proj, declared) {
                        (Inv, d) => *d,
                        (p, _) => *p,
                    };
                    first_violation(table, t, pos.compose(arg), params, alias)
                }
            })
        }
        TypeExpr::Func {
            recv,
            params: ps,
            ret,
            ..
        } => recv
            .iter()
            .map(|r| &**r)
            .chain(ps.iter())
            .find_map(|p| first_violation(table, p, pos.compose(In), params, false))
            .or_else(|| first_violation(table, ret, pos, params, false)),
    }
}

/// Reject a class member whose written type puts one of the class's `out`
/// type parameters in an `in` or invariant position, or an `in` one in an
/// `out` or invariant position — with `kotlinc`'s message. `private` members
/// are exempt, as they are on the reference.
pub fn check_declaration_variance(program: &Program) -> Result<(), String> {
    let table = VarianceTable::new(program);
    for c in &program.classes {
        let Some(header) = &c.variance_header else {
            continue;
        };
        if c.variance.iter().all(|v| *v == Inv) {
            continue;
        }
        let all: Vec<(String, Variance)> = c
            .type_params
            .iter()
            .cloned()
            .zip(c.variance.iter().copied())
            .collect();
        for m in c.member_types.iter().filter(|m| !m.private) {
            let mut sites: Vec<(&TypeExpr, Variance, Params)> = Vec::new();
            match &m.kind {
                MemberKind::Prop { mutable, ty } => {
                    if let Some(t) = ty {
                        let pos = if *mutable { Inv } else { Out };
                        sites.push((t, pos, all.clone()));
                    }
                }
                MemberKind::Fun {
                    own_tps,
                    recv,
                    params,
                    ret,
                } => {
                    let visible: Vec<(String, Variance)> = all
                        .iter()
                        .filter(|(n, _)| !own_tps.contains(n))
                        .cloned()
                        .collect();
                    for t in recv
                        .iter()
                        .chain(params.iter().filter_map(|(_, t, _)| t.as_ref()))
                    {
                        sites.push((t, In, visible.clone()));
                    }
                    if let Some(r) = ret {
                        sites.push((r, Out, visible.clone()));
                    }
                }
            }
            for (t, pos, visible) in sites {
                if let Some(v) = first_violation(&table, t, pos, &visible, false) {
                    let label = |n: &str| {
                        visible
                            .iter()
                            .any(|(p, _)| p == n)
                            .then(|| format!("{n} (of {header})"))
                    };
                    return Err(format!(
                        "type parameter '{}' is declared as '{}' but occurs in '{}' position in {}type '{}'.",
                        v.name,
                        v.declared.word(),
                        v.pos.word(),
                        if v.abbreviated { "abbreviated " } else { "" },
                        t.render_with(&label)
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The type a standard-library factory call returns, by the callee's name —
/// `mutableListOf<String>()` is a `MutableList<String>`. Only factories whose
/// type arguments ARE the result's are listed.
fn factory_result(callee: &str) -> Option<&'static str> {
    Some(match callee {
        "listOf" | "emptyList" | "listOfNotNull" => "List",
        "mutableListOf" => "MutableList",
        "arrayListOf" => "ArrayList",
        "setOf" | "emptySet" => "Set",
        "mutableSetOf" => "MutableSet",
        "hashSetOf" => "HashSet",
        "linkedSetOf" => "LinkedHashSet",
        "mapOf" | "emptyMap" => "Map",
        "mutableMapOf" => "MutableMap",
        "hashMapOf" => "HashMap",
        "linkedMapOf" => "LinkedHashMap",
        "arrayOf" | "emptyArray" => "Array",
        _ => return None,
    })
}

/// The simple types no other type is a subtype of, and that are subtypes of
/// nothing but themselves, `Any`, and (for the numbers) `Number` and
/// `Comparable`. Two of these that differ are certainly unrelated.
fn is_final_builtin(head: &str) -> bool {
    matches!(
        head,
        "String"
            | "Int"
            | "Long"
            | "Short"
            | "Byte"
            | "Double"
            | "Float"
            | "Boolean"
            | "Char"
            | "Unit"
    )
}

fn is_number(head: &str) -> bool {
    matches!(head, "Int" | "Long" | "Short" | "Byte" | "Double" | "Float")
}

/// Fold per-argument answers: any certain `false` decides, otherwise every
/// argument must be a certain `true`.
fn all_certain(answers: impl Iterator<Item = Option<bool>>) -> Option<bool> {
    let mut unknown = false;
    for a in answers {
        match a {
            Some(false) => return Some(false),
            None => unknown = true,
            Some(true) => {}
        }
    }
    (!unknown).then_some(true)
}

impl VarianceTable {
    /// The written type of a construction with explicit type arguments — the
    /// callee as the type it produces — when the callee is certainly a
    /// constructor or a listed factory.
    pub fn construction_type(&self, call: &TypeExpr) -> Option<TypeExpr> {
        let TypeExpr::Named {
            name,
            args,
            nullable,
        } = call
        else {
            return None;
        };
        if name.contains('.') || self.funs.contains(name) {
            return None;
        }
        let result = match factory_result(name) {
            Some(r) if !self.user.contains_key(name) => r.to_string(),
            _ if self.user_class(call).is_some() => name.clone(),
            _ if stdlib_variance(name).is_some() => name.clone(),
            _ => return None,
        };
        Some(TypeExpr::Named {
            name: result,
            args: args.clone(),
            nullable: *nullable,
        })
    }

    /// Whether `a` and `d` are certainly the same type (`Some(true)`),
    /// certainly different (`Some(false)`), or not decidable here.
    fn same(&self, a: &TypeExpr, d: &TypeExpr) -> Option<bool> {
        match (a, d) {
            (
                TypeExpr::Named {
                    name: an,
                    args: aa,
                    nullable: aq,
                },
                TypeExpr::Named {
                    name: dn,
                    args: da,
                    nullable: dq,
                },
            ) => {
                if an.contains('.') || dn.contains('.') {
                    return None;
                }
                if an != dn || aq != dq || aa.len() != da.len() {
                    return Some(false);
                }
                all_certain(aa.iter().zip(da).map(|(x, y)| match (x, y) {
                    (TypeProj::Star, TypeProj::Star) => Some(true),
                    (TypeProj::Arg(xv, xt), TypeProj::Arg(yv, yt)) if xv == yv => self.same(xt, yt),
                    _ => Some(false),
                }))
            }
            _ => None,
        }
    }

    /// Whether `a` is certainly a subtype of `d`, certainly not, or unknown.
    fn subtype(&self, a: &TypeExpr, d: &TypeExpr) -> Option<bool> {
        let (
            TypeExpr::Named {
                name: an,
                args: aa,
                nullable: aq,
            },
            TypeExpr::Named {
                name: dn,
                args: da,
                nullable: dq,
            },
        ) = (a, d)
        else {
            return None;
        };
        if an.contains('.') || dn.contains('.') {
            return None;
        }
        if *aq && !*dq {
            return Some(false);
        }
        if dn == "Any" && da.is_empty() {
            return Some(true);
        }
        if an != dn {
            if self.names_user_class(a) || self.names_user_class(d) {
                return None;
            }
            let (ah, dh) = (an.as_str(), dn.as_str());
            return match (is_final_builtin(ah), is_final_builtin(dh)) {
                (true, true) => Some(false),
                (true, false) if dh == "Number" => Some(is_number(ah)),
                (false, true) if ah == "Number" => Some(false),
                _ => None,
            };
        }
        if aa.len() != da.len() {
            return None;
        }
        if aa.is_empty() {
            return Some(true);
        }
        let vs = self.of(d)?;
        all_certain(aa.iter().zip(da).zip(vs).map(|((x, y), declared)| {
            let TypeProj::Arg(Variance::Invariant, xt) = x else {
                return None;
            };
            let TypeProj::Arg(proj, yt) = y else {
                return Some(true); // `*` accepts every argument
            };
            match if *proj == Variance::Invariant {
                *declared
            } else {
                *proj
            } {
                Variance::Invariant => self.same(xt, yt),
                Variance::Out => self.subtype(xt, yt),
                Variance::In => self.subtype(yt, xt),
            }
        }))
    }

    /// `kotlinc`'s message for `val x: <declared> = <actual>` when `actual`
    /// is certainly not a subtype of `declared`.
    pub fn initializer_mismatch(&self, declared: &TypeExpr, actual: &TypeExpr) -> Option<String> {
        (self.subtype(actual, declared) == Some(false)).then(|| {
            format!(
                "initializer type mismatch: expected '{}', actual '{}'.",
                declared.render(),
                actual.render()
            )
        })
    }

    /// The projection a receiver puts on the class type parameter named
    /// `tp`: the written argument at its index.
    fn projection_of<'t>(
        &self,
        recv: &'t TypeExpr,
        info: &ClassVariance,
        tp: &str,
    ) -> Option<&'t TypeProj> {
        let TypeExpr::Named { args, .. } = recv else {
            return None;
        };
        let i = info.type_params.iter().position(|p| p == tp)?;
        (args.len() == info.type_params.len()).then(|| &args[i])
    }

    /// The single member named `name` of the receiver's user class.
    fn sole_member<'c>(info: &'c ClassVariance, name: &str) -> Option<&'c MemberType> {
        let mut it = info.members.iter().filter(|m| m.name == name);
        let m = it.next()?;
        it.next().is_none().then_some(m)
    }

    /// `kotlinc`'s refusal of a call through an `out`- or star-projected
    /// receiver to a member that takes the projected type parameter:
    /// `receiver type 'Box<out Number>' contains out projection which
    /// prohibits the use of 'fun put(x: T): Unit'.` `argc` is the number of
    /// arguments the call passes; an indexed write is a `set` with two.
    pub fn projected_call(
        &self,
        recv: &TypeExpr,
        method: &str,
        argc: usize,
        indexed: bool,
    ) -> Option<String> {
        let TypeExpr::Named { args, .. } = recv else {
            return None;
        };
        let word = |p: &TypeProj| match p {
            TypeProj::Star => Some("star"),
            TypeProj::Arg(Variance::Out, _) => Some("out"),
            _ => None,
        };
        let refuse = |kind: &str, sig: &str| {
            format!(
                "receiver type '{}' contains {kind} projection which prohibits the use of '{sig}'.",
                recv.render()
            )
        };
        if self.names_user_class(recv) {
            let info = self.user_class(recv)?;
            let m = Self::sole_member(info, method)?;
            let MemberKind::Fun {
                own_tps,
                recv: None,
                params,
                ret: Some(ret),
            } = &m.kind
            else {
                return None;
            };
            if !own_tps.is_empty() || m.private {
                return None;
            }
            let mut kind = None;
            for (_, t, _) in params {
                let TypeExpr::Named {
                    name, args: targs, ..
                } = t.as_ref()?
                else {
                    continue;
                };
                if !targs.is_empty() {
                    continue;
                }
                if let Some(p) = self.projection_of(recv, info, name) {
                    kind = kind.or(word(p));
                }
            }
            let rendered: Vec<String> = params
                .iter()
                .map(|(n, t, d)| {
                    let t = t.as_ref().map(|t| t.render()).unwrap_or_default();
                    if *d {
                        format!("{n}: {t} = ...")
                    } else {
                        format!("{n}: {t}")
                    }
                })
                .collect();
            let sig = format!("fun {method}({}): {}", rendered.join(", "), ret.render());
            return kind.map(|k| refuse(k, &sig));
        }
        let head = recv.head()?;
        let java = matches!(
            head,
            "ArrayList" | "HashSet" | "LinkedHashSet" | "HashMap" | "LinkedHashMap"
        );
        let (projected, sig) = match (head, method, argc) {
            (
                "MutableList" | "MutableSet" | "MutableCollection" | "ArrayList" | "HashSet"
                | "LinkedHashSet",
                "add",
                1,
            ) => (
                args.first()?,
                if java {
                    "fun add(p0: E): Boolean"
                } else {
                    "fun add(element: E): Boolean"
                },
            ),
            ("MutableList" | "ArrayList", "add", 2) => (
                args.first()?,
                if java {
                    "fun add(p0: Int, p1: E): Unit"
                } else {
                    "fun add(index: Int, element: E): Unit"
                },
            ),
            ("MutableList" | "ArrayList", "set", 2) => (
                args.first()?,
                if java {
                    "fun set(p0: Int, p1: E): E"
                } else {
                    "fun set(index: Int, element: E): E"
                },
            ),
            ("Array", "set", 2) => (args.first()?, "fun set(index: Int, value: T): Unit"),
            ("MutableMap" | "HashMap" | "LinkedHashMap", "set", 2) if indexed => (
                args.iter().find(|a| word(a).is_some())?,
                "fun <K, V> MutableMap<K, V>.set(key: K, value: V): Unit",
            ),
            ("MutableMap" | "HashMap" | "LinkedHashMap", "put", 2) => (
                args.iter().find(|a| word(a).is_some())?,
                if java {
                    "fun put(p0: K, p1: V): V?"
                } else {
                    "fun put(key: K, value: V): V?"
                },
            ),
            _ => return None,
        };
        if args.len() != stdlib_variance(head)?.len() {
            return None;
        }
        word(projected).map(|k| refuse(k, sig))
    }

    /// The type `kotlinc` gives a read through an `in`- or star-projected
    /// receiver of a member typed by the bare type parameter — the
    /// `CapturedType(in Int)` / `CapturedType(*)` a declaration then fails to
    /// accept. `member` is a property name, a method name, or `get` for an
    /// index read.
    pub fn projected_read(
        &self,
        recv: &TypeExpr,
        member: &str,
        call: bool,
        indexed: bool,
    ) -> Option<String> {
        let TypeExpr::Named { args, .. } = recv else {
            return None;
        };
        let captured = |p: &TypeProj, nullable: bool| {
            let q = if nullable { "?" } else { "" };
            match p {
                TypeProj::Star => Some(format!("CapturedType(*){q}")),
                TypeProj::Arg(Variance::In, t) => {
                    Some(format!("CapturedType(in {}){q}", t.render()))
                }
                _ => None,
            }
        };
        if self.names_user_class(recv) {
            let info = self.user_class(recv)?;
            if info.variance.iter().any(|v| *v != Variance::Invariant) {
                return None;
            }
            let m = Self::sole_member(info, member)?;
            let ty = match (&m.kind, call) {
                (MemberKind::Prop { ty: Some(t), .. }, false) => t,
                (
                    MemberKind::Fun {
                        own_tps,
                        recv: None,
                        params,
                        ret: Some(r),
                    },
                    true,
                ) if own_tps.is_empty() && params.is_empty() => r,
                _ => return None,
            };
            let TypeExpr::Named {
                name,
                args: targs,
                nullable,
            } = ty
            else {
                return None;
            };
            if !targs.is_empty() {
                return None;
            }
            return captured(self.projection_of(recv, info, name)?, *nullable);
        }
        let head = recv.head()?;
        let reads = (indexed || (call && member == "get"))
            && matches!(head, "MutableList" | "ArrayList" | "Array");
        if !reads || args.len() != 1 {
            return None;
        }
        captured(&args[0], false)
    }
}

impl VarianceTable {
    /// Whether `recv.name = …` writes a `var` of a type parameter that the
    /// receiver projects `out` or `*` — a setter that takes `Nothing`.
    pub fn projected_write(&self, recv: &TypeExpr, name: &str) -> bool {
        let Some(info) = self
            .user_class(recv)
            .filter(|_| self.names_user_class(recv))
        else {
            return false;
        };
        let Some(MemberType {
            kind:
                MemberKind::Prop {
                    mutable: true,
                    ty: Some(TypeExpr::Named { name: tp, args, .. }),
                },
            private: false,
            ..
        }) = Self::sole_member(info, name)
        else {
            return false;
        };
        args.is_empty()
            && matches!(
                self.projection_of(recv, info, tp),
                Some(TypeProj::Star | TypeProj::Arg(Variance::Out, _))
            )
    }
}
