//! Tokens to a syntax tree: recursive descent, with operator precedence for
//! binary expressions.
//!
//! Generators and modules are not supported; a script that uses them fails
//! to parse and is skipped.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::ast::*;
use crate::lexer::{Lexer, Tok, Token};

type R<T> = Result<T, String>;

const RESERVED: &[&str] = &[
    "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do", "else", "export", "extends", "finally", "for", "function", "if", "import", "in", "instanceof", "new", "return", "super", "switch", "this", "throw", "try", "typeof", "var", "void", "while", "with", "null", "true", "false", "enum",
];

/// How deep expressions and statements may nest.
const MAX_DEPTH: u32 = 160;

/// What the parser knows about the function it is in.
#[derive(Default)]
struct Ctx {
    vars: Vec<Name>,
    uses_args: bool,
    arrow: bool,
    is_async: bool,
}

pub struct Parser {
    t: Vec<Token>,
    p: usize,
    /// For each `(` token, the index of its `)`.
    close: Vec<usize>,
    fns: Vec<Ctx>,
    no_in: bool,
    depth: u32,
}

/// Parses a whole script.
pub fn parse(src: &str) -> R<Program> {
    let mut p = Parser::new(src, false)?;
    let mut body = Vec::new();
    while !matches!(p.peek(), Tok::Eof) {
        body.push(p.stmt()?);
    }
    let ctx = p.fns.pop().unwrap_or_default();
    Ok(Program { body, vars: ctx.vars })
}

fn matching_parens(t: &[Token]) -> Vec<usize> {
    let mut close = vec![0; t.len()];
    let mut stack = Vec::new();
    for (i, tok) in t.iter().enumerate() {
        match tok.tok {
            Tok::Punct("(") => stack.push(i),
            Tok::Punct(")") => {
                if let Some(o) = stack.pop() {
                    close[o] = i;
                }
            }
            _ => {}
        }
    }
    close
}

fn is_reserved(n: &str) -> bool {
    RESERVED.contains(&n)
}

fn assign_op(p: &str) -> Option<Option<Op>> {
    Some(match p {
        "=" => None,
        "+=" => Some(Op::Add),
        "-=" => Some(Op::Sub),
        "*=" => Some(Op::Mul),
        "/=" => Some(Op::Div),
        "%=" => Some(Op::Mod),
        "**=" => Some(Op::Exp),
        "<<=" => Some(Op::Shl),
        ">>=" => Some(Op::Shr),
        ">>>=" => Some(Op::UShr),
        "&=" => Some(Op::BitAnd),
        "|=" => Some(Op::BitOr),
        "^=" => Some(Op::BitXor),
        "&&=" => Some(Op::And),
        "||=" => Some(Op::Or),
        "??=" => Some(Op::Nullish),
        _ => return None,
    })
}

fn binary_op(p: &str) -> Option<(Op, u8)> {
    Some(match p {
        "??" => (Op::Nullish, 1),
        "||" => (Op::Or, 2),
        "&&" => (Op::And, 3),
        "|" => (Op::BitOr, 4),
        "^" => (Op::BitXor, 5),
        "&" => (Op::BitAnd, 6),
        "==" => (Op::Eq, 7),
        "!=" => (Op::Ne, 7),
        "===" => (Op::StrictEq, 7),
        "!==" => (Op::StrictNe, 7),
        "<" => (Op::Lt, 8),
        ">" => (Op::Gt, 8),
        "<=" => (Op::Le, 8),
        ">=" => (Op::Ge, 8),
        "<<" => (Op::Shl, 9),
        ">>" => (Op::Shr, 9),
        ">>>" => (Op::UShr, 9),
        "+" => (Op::Add, 10),
        "-" => (Op::Sub, 10),
        "*" => (Op::Mul, 11),
        "/" => (Op::Div, 11),
        "%" => (Op::Mod, 11),
        "**" => (Op::Exp, 12),
        _ => return None,
    })
}

/// The names a pattern binds.
fn pat_names(p: &Pat, out: &mut Vec<Name>) {
    match p {
        Pat::Id(n) => out.push(n.clone()),
        Pat::Target(_) => {}
        Pat::Arr(items, rest) => {
            for i in items.iter().flatten() {
                pat_names(i, out);
            }
            if let Some(r) = rest {
                pat_names(r, out);
            }
        }
        Pat::Obj(props, rest) => {
            for (_, i) in props {
                pat_names(i, out);
            }
            if let Some(r) = rest {
                pat_names(r, out);
            }
        }
        Pat::Def(p, _) => pat_names(p, out),
    }
}

impl Parser {
    fn new(src: &str, is_async: bool) -> R<Self> {
        let t = Lexer::new(src).tokens()?;
        let close = matching_parens(&t);
        Ok(Parser { t, p: 0, close, fns: vec![Ctx { is_async, ..Ctx::default() }], no_in: false, depth: 0 })
    }

    // -- Tokens -----------------------------------------------------------

    fn cur(&self) -> &Token {
        &self.t[self.p.min(self.t.len() - 1)]
    }

    fn peek(&self) -> &Tok {
        &self.cur().tok
    }

    fn peek_at(&self, k: usize) -> &Tok {
        &self.t[(self.p + k).min(self.t.len() - 1)].tok
    }

    fn nl_at(&self, k: usize) -> bool {
        self.t[(self.p + k).min(self.t.len() - 1)].nl
    }

    fn line(&self) -> u32 {
        self.cur().line
    }

    fn next(&mut self) -> Tok {
        let t = self.cur().tok.clone();
        self.p += 1;
        t
    }

    fn is(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::Punct(q) if *q == p)
    }

    fn is_name(&self, n: &str) -> bool {
        matches!(self.peek(), Tok::Name(m) if &**m == n)
    }

    fn eat(&mut self, p: &str) -> bool {
        if self.is(p) {
            self.next();
            true
        } else {
            false
        }
    }

    fn eat_name(&mut self, n: &str) -> bool {
        if self.is_name(n) {
            self.next();
            true
        } else {
            false
        }
    }

    fn err<T>(&self, what: &str) -> R<T> {
        let found = match self.peek() {
            Tok::Name(n) => format!("'{n}'"),
            Tok::Num(n) => format!("{n}"),
            Tok::Str(_) => String::from("una stringa"),
            Tok::Tmpl(..) => String::from("un template"),
            Tok::Regex(..) => String::from("un'espressione regolare"),
            Tok::Punct(p) => format!("'{p}'"),
            Tok::Eof => String::from("la fine"),
        };
        Err(format!("riga {}: {what} (trovato {found})", self.line()))
    }

    fn expect(&mut self, p: &str) -> R<()> {
        if self.eat(p) {
            Ok(())
        } else {
            self.err(&format!("atteso '{p}'"))
        }
    }

    /// An identifier that may name a binding.
    fn ident(&mut self) -> R<Name> {
        match self.peek().clone() {
            Tok::Name(n) if !is_reserved(&n) => {
                self.next();
                Ok(n)
            }
            _ => self.err("atteso un nome"),
        }
    }

    /// Ends a statement, allowing for automatic semicolons.
    fn semi(&mut self) -> R<()> {
        if self.eat(";") || self.is("}") || matches!(self.peek(), Tok::Eof) || self.cur().nl {
            Ok(())
        } else {
            self.err("atteso ';'")
        }
    }

    fn ctx(&mut self) -> &mut Ctx {
        self.fns.last_mut().expect("function context")
    }

    fn mark_arguments(&mut self) {
        if let Some(c) = self.fns.iter_mut().rev().find(|c| !c.arrow) {
            c.uses_args = true;
        }
    }

    fn deeper(&mut self) -> R<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err("annidamento troppo profondo");
        }
        Ok(())
    }

    // -- Statements -------------------------------------------------------

    fn stmt(&mut self) -> R<Stmt> {
        self.deeper()?;
        let s = self.stmt_inner();
        self.depth -= 1;
        s
    }

    fn stmt_inner(&mut self) -> R<Stmt> {
        let tok = self.peek().clone();
        match tok {
            Tok::Punct("{") => {
                let body = self.block()?;
                let lexical = body.iter().any(Stmt::lexical);
                Ok(Stmt::Block(body, lexical))
            }
            Tok::Punct(";") => {
                self.next();
                Ok(Stmt::Empty)
            }
            Tok::Name(n) => match &*n {
                "var" | "const" => {
                    self.next();
                    let kind = if &*n == "var" { DeclKind::Var } else { DeclKind::Const };
                    let d = self.decl(kind)?;
                    self.semi()?;
                    Ok(d)
                }
                "let" if matches!(self.peek_at(1), Tok::Name(_) | Tok::Punct("[" | "{")) => {
                    self.next();
                    let d = self.decl(DeclKind::Let)?;
                    self.semi()?;
                    Ok(d)
                }
                "function" => {
                    self.next();
                    let name = self.ident()?;
                    Ok(Stmt::Func(self.function(Some(name), false, false)?))
                }
                "async" if matches!(self.peek_at(1), Tok::Name(f) if &**f == "function") && !self.nl_at(1) => {
                    self.next();
                    self.next();
                    let name = self.ident()?;
                    Ok(Stmt::Func(self.function(Some(name), true, false)?))
                }
                "class" => Ok(Stmt::Class(self.class()?)),
                "if" => {
                    self.next();
                    self.expect("(")?;
                    let c = self.expr()?;
                    self.expect(")")?;
                    let then = Box::new(self.stmt()?);
                    let other = if self.eat_name("else") { Some(Box::new(self.stmt()?)) } else { None };
                    Ok(Stmt::If(c, then, other))
                }
                "for" => self.for_stmt(),
                "while" => {
                    self.next();
                    self.expect("(")?;
                    let c = self.expr()?;
                    self.expect(")")?;
                    Ok(Stmt::While(c, Box::new(self.stmt()?)))
                }
                "do" => {
                    self.next();
                    let body = Box::new(self.stmt()?);
                    if !self.eat_name("while") {
                        return self.err("atteso 'while'");
                    }
                    self.expect("(")?;
                    let c = self.expr()?;
                    self.expect(")")?;
                    self.eat(";");
                    Ok(Stmt::DoWhile(body, c))
                }
                "return" => {
                    self.next();
                    let v = if self.is(";") || self.is("}") || self.cur().nl || matches!(self.peek(), Tok::Eof) { None } else { Some(self.expr()?) };
                    self.semi()?;
                    Ok(Stmt::Return(v))
                }
                "break" | "continue" => {
                    self.next();
                    let label = match self.peek().clone() {
                        Tok::Name(l) if !self.cur().nl && !is_reserved(&l) => {
                            self.next();
                            Some(l)
                        }
                        _ => None,
                    };
                    self.semi()?;
                    Ok(if &*n == "break" { Stmt::Break(label) } else { Stmt::Continue(label) })
                }
                "throw" => {
                    self.next();
                    let v = self.expr()?;
                    self.semi()?;
                    Ok(Stmt::Throw(v))
                }
                "try" => self.try_stmt(),
                "switch" => self.switch_stmt(),
                "debugger" => {
                    self.next();
                    self.semi()?;
                    Ok(Stmt::Empty)
                }
                "import" if !matches!(self.peek_at(1), Tok::Punct("(" | ".")) => self.err("i moduli non sono supportati"),
                "export" => self.err("i moduli non sono supportati"),
                _ if matches!(self.peek_at(1), Tok::Punct(":")) && !is_reserved(&n) => {
                    self.next();
                    self.next();
                    Ok(Stmt::Label(n, Box::new(self.stmt()?)))
                }
                _ => self.expr_stmt(),
            },
            _ => self.expr_stmt(),
        }
    }

    fn expr_stmt(&mut self) -> R<Stmt> {
        let e = self.expr()?;
        self.semi()?;
        Ok(Stmt::Expr(e))
    }

    fn block(&mut self) -> R<Vec<Stmt>> {
        self.expect("{")?;
        let mut body = Vec::new();
        while !self.eat("}") {
            if matches!(self.peek(), Tok::Eof) {
                return self.err("atteso '}'");
            }
            body.push(self.stmt()?);
        }
        Ok(body)
    }

    fn decl(&mut self, kind: DeclKind) -> R<Stmt> {
        let mut list = Vec::new();
        loop {
            let pat = self.binding_pat()?;
            if kind == DeclKind::Var {
                let mut names = Vec::new();
                pat_names(&pat, &mut names);
                self.ctx().vars.extend(names);
            }
            let init = if self.eat("=") { Some(self.assign()?) } else { None };
            list.push((pat, init));
            if !self.eat(",") {
                break;
            }
        }
        Ok(Stmt::Decl(kind, list))
    }

    fn for_stmt(&mut self) -> R<Stmt> {
        self.next();
        if self.is_name("await") {
            return self.err("'for await' non è supportato");
        }
        self.expect("(")?;
        let save = self.no_in;
        self.no_in = true;
        let init: Option<Box<Stmt>> = if self.is(";") {
            None
        } else {
            let kind = match self.peek() {
                Tok::Name(n) if &**n == "var" => Some(DeclKind::Var),
                Tok::Name(n) if &**n == "const" => Some(DeclKind::Const),
                Tok::Name(n) if &**n == "let" && matches!(self.peek_at(1), Tok::Name(_) | Tok::Punct("[" | "{")) => Some(DeclKind::Let),
                _ => None,
            };
            if let Some(k) = kind {
                self.next();
                Some(Box::new(self.decl(k)?))
            } else {
                Some(Box::new(Stmt::Expr(self.expr()?)))
            }
        };
        self.no_in = save;
        let of = self.is_name("of");
        if of || self.is_name("in") {
            self.next();
            let (kind, pat) = match init.map(|b| *b) {
                Some(Stmt::Decl(k, mut list)) if list.len() == 1 => (Some(k), list.pop().map(|d| d.0).expect("one declaration")),
                Some(Stmt::Expr(e)) => (None, self.to_pat(e)?),
                _ => return self.err("ciclo for non valido"),
            };
            let src = if of { self.assign()? } else { self.expr()? };
            self.expect(")")?;
            let body = Box::new(self.stmt()?);
            return Ok(if of { Stmt::ForOf(kind, pat, src, body) } else { Stmt::ForIn(kind, pat, src, body) });
        }
        self.expect(";")?;
        let test = if self.is(";") { None } else { Some(self.expr()?) };
        self.expect(";")?;
        let update = if self.is(")") { None } else { Some(self.expr()?) };
        self.expect(")")?;
        let body = Box::new(self.stmt()?);
        Ok(Stmt::For(init, test, update, body))
    }

    fn try_stmt(&mut self) -> R<Stmt> {
        self.next();
        let body = self.block()?;
        let catch = if self.eat_name("catch") {
            let param = if self.eat("(") {
                let p = self.binding_pat()?;
                self.expect(")")?;
                Some(p)
            } else {
                None
            };
            Some((param, self.block()?))
        } else {
            None
        };
        let finally = if self.eat_name("finally") { Some(self.block()?) } else { None };
        if catch.is_none() && finally.is_none() {
            return self.err("atteso 'catch' o 'finally'");
        }
        Ok(Stmt::Try(body, catch, finally))
    }

    fn switch_stmt(&mut self) -> R<Stmt> {
        self.next();
        self.expect("(")?;
        let d = self.expr()?;
        self.expect(")")?;
        self.expect("{")?;
        let mut cases = Vec::new();
        while !self.eat("}") {
            let test = if self.eat_name("case") {
                Some(self.expr()?)
            } else if self.eat_name("default") {
                None
            } else {
                return self.err("atteso 'case'");
            };
            self.expect(":")?;
            let mut body = Vec::new();
            while !self.is("}") && !self.is_name("case") && !self.is_name("default") {
                if matches!(self.peek(), Tok::Eof) {
                    return self.err("atteso '}'");
                }
                body.push(self.stmt()?);
            }
            cases.push((test, body));
        }
        Ok(Stmt::Switch(d, cases))
    }

    // -- Functions and classes --------------------------------------------

    /// Parameters and body, after the name.
    fn function(&mut self, name: Option<Name>, is_async: bool, method: bool) -> R<Rc<FuncDef>> {
        if self.is("*") {
            return self.err("i generatori non sono supportati");
        }
        let line = self.line();
        self.fns.push(Ctx { is_async, ..Ctx::default() });
        let save = self.no_in;
        self.no_in = false;
        let params = self.params();
        let body = params.and_then(|p| self.block().map(|b| (p, b)));
        self.no_in = save;
        let ctx = self.fns.pop().expect("function context");
        let ((params, rest), body) = body?;
        Ok(Rc::new(FuncDef { name, params, rest, body: Body::Block(body), arrow: false, is_async, method, vars: ctx.vars, uses_args: ctx.uses_args, line }))
    }

    fn params(&mut self) -> R<(Vec<Pat>, Option<Pat>)> {
        self.expect("(")?;
        let mut params = Vec::new();
        let mut rest = None;
        while !self.eat(")") {
            if self.eat("...") {
                rest = Some(self.binding_pat()?);
                self.eat(",");
                self.expect(")")?;
                break;
            }
            params.push(self.binding_elem()?);
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        Ok((params, rest))
    }

    fn arrow(&mut self, is_async: bool) -> R<Expr> {
        let line = self.line();
        self.fns.push(Ctx { arrow: true, is_async, ..Ctx::default() });
        let r = (|| -> R<_> {
            let (params, rest) = if self.is("(") { self.params()? } else { (vec![Pat::Id(self.ident()?)], None) };
            self.expect("=>")?;
            let body = if self.is("{") {
                let save = self.no_in;
                self.no_in = false;
                let b = self.block();
                self.no_in = save;
                Body::Block(b?)
            } else {
                Body::Expr(self.assign()?)
            };
            Ok((params, rest, body))
        })();
        let ctx = self.fns.pop().expect("function context");
        let (params, rest, body) = r?;
        Ok(Expr::Func(Rc::new(FuncDef { name: None, params, rest, body, arrow: true, is_async, method: false, vars: ctx.vars, uses_args: false, line })))
    }

    fn class(&mut self) -> R<Rc<ClassDef>> {
        self.next();
        let name = match self.peek() {
            Tok::Name(n) if &**n != "extends" => Some(self.ident()?),
            _ => None,
        };
        let extends = if self.eat_name("extends") { Some(self.call_member()?) } else { None };
        self.expect("{")?;
        let mut members = Vec::new();
        let mut ctor = None;
        while !self.eat("}") {
            if self.eat(";") {
                continue;
            }
            let is_static = self.is_name("static") && !matches!(self.peek_at(1), Tok::Punct("(" | "=" | ";" | "}"));
            if is_static {
                self.next();
                if self.is("{") {
                    let line = self.line();
                    self.fns.push(Ctx::default());
                    let body = self.block();
                    let ctx = self.fns.pop().expect("function context");
                    let f = FuncDef { name: None, params: Vec::new(), rest: None, body: Body::Block(body?), arrow: false, is_async: false, method: true, vars: ctx.vars, uses_args: false, line };
                    members.push(ClassMember { key: PropName::Name(Rc::from("")), is_static, kind: MemberKind::Block(Rc::new(f)) });
                    continue;
                }
            }
            let (accessor, is_async) = self.method_prefix();
            let key = self.prop_name()?;
            if self.is("(") {
                let fname = match &key {
                    PropName::Name(n) => Some(n.clone()),
                    PropName::Computed(_) => None,
                };
                let f = self.function(fname, is_async, true)?;
                let is_ctor = !is_static && accessor.is_none() && matches!(&key, PropName::Name(n) if &**n == "constructor");
                if is_ctor {
                    ctor = Some(f);
                    continue;
                }
                let kind = match accessor {
                    Some(true) => MemberKind::Get(f),
                    Some(false) => MemberKind::Set(f),
                    None => MemberKind::Method(f),
                };
                members.push(ClassMember { key, is_static, kind });
            } else {
                // Initialisers see `this` but not the enclosing `arguments`.
                self.fns.push(Ctx { arrow: false, ..Ctx::default() });
                let init = if self.eat("=") { self.assign().map(Some) } else { Ok(None) };
                self.fns.pop();
                let init = init?;
                self.semi()?;
                members.push(ClassMember { key, is_static, kind: MemberKind::Field(init) });
            }
        }
        let ctor = match ctor {
            Some(c) => c,
            None => {
                let line = self.line();
                let body = if extends.is_some() {
                    let args: Name = Rc::from("args");
                    vec![Stmt::Expr(Expr::SuperCall(vec![Expr::Spread(Box::new(Expr::Ident(args)))]))]
                } else {
                    Vec::new()
                };
                let rest = extends.as_ref().map(|_| Pat::Id(Rc::from("args")));
                Rc::new(FuncDef { name: name.clone(), params: Vec::new(), rest, body: Body::Block(body), arrow: false, is_async: false, method: false, vars: Vec::new(), uses_args: false, line })
            }
        };
        Ok(Rc::new(ClassDef { name, extends, ctor, members }))
    }

    /// `get`, `set` and `async` before a method name: (getter?, async).
    fn method_prefix(&mut self) -> (Option<bool>, bool) {
        let follows_key = |p: &Parser| !matches!(p.peek_at(1), Tok::Punct("(" | "," | ":" | "}" | "=" | ";")) && !matches!(p.peek_at(1), Tok::Eof);
        if (self.is_name("get") || self.is_name("set")) && follows_key(self) {
            let get = self.is_name("get");
            self.next();
            return (Some(get), false);
        }
        if self.is_name("async") && follows_key(self) && !self.nl_at(1) {
            self.next();
            return (None, true);
        }
        (None, false)
    }

    fn prop_name(&mut self) -> R<PropName> {
        match self.next() {
            Tok::Name(n) => Ok(PropName::Name(n)),
            Tok::Str(s) => Ok(PropName::Name(s)),
            Tok::Num(n) => Ok(PropName::Name(Rc::from(crate::value::num_to_string(n).as_str()))),
            Tok::Punct("[") => {
                let save = self.no_in;
                self.no_in = false;
                let e = self.assign();
                self.no_in = save;
                let e = e?;
                self.expect("]")?;
                Ok(PropName::Computed(e))
            }
            Tok::Punct("*") => self.err("i generatori non sono supportati"),
            _ => {
                self.p -= 1;
                self.err("atteso il nome di una proprietà")
            }
        }
    }

    // -- Patterns ---------------------------------------------------------

    fn binding_elem(&mut self) -> R<Pat> {
        let p = self.binding_pat()?;
        if self.eat("=") {
            let save = self.no_in;
            self.no_in = false;
            let d = self.assign();
            self.no_in = save;
            return Ok(Pat::Def(Box::new(p), d?));
        }
        Ok(p)
    }

    fn binding_pat(&mut self) -> R<Pat> {
        if self.eat("[") {
            let mut items = Vec::new();
            let mut rest = None;
            while !self.eat("]") {
                if self.eat(",") {
                    items.push(None);
                    continue;
                }
                if self.eat("...") {
                    rest = Some(Box::new(self.binding_pat()?));
                    self.eat(",");
                    self.expect("]")?;
                    break;
                }
                items.push(Some(self.binding_elem()?));
                if !self.is("]") {
                    self.expect(",")?;
                }
            }
            return Ok(Pat::Arr(items, rest));
        }
        if self.eat("{") {
            let mut props = Vec::new();
            let mut rest = None;
            while !self.eat("}") {
                if self.eat("...") {
                    rest = Some(Box::new(Pat::Id(self.ident()?)));
                    self.eat(",");
                    self.expect("}")?;
                    break;
                }
                let key = self.prop_name()?;
                let value = if self.eat(":") {
                    self.binding_elem()?
                } else {
                    let PropName::Name(n) = &key else { return self.err("atteso ':'") };
                    let id = Pat::Id(n.clone());
                    if self.eat("=") {
                        Pat::Def(Box::new(id), self.assign()?)
                    } else {
                        id
                    }
                };
                props.push((key, value));
                if !self.is("}") {
                    self.expect(",")?;
                }
            }
            return Ok(Pat::Obj(props, rest));
        }
        Ok(Pat::Id(self.ident()?))
    }

    /// An expression on the left of `=` as a pattern.
    fn to_pat(&self, e: Expr) -> R<Pat> {
        Ok(match e {
            Expr::Ident(n) => Pat::Id(n),
            e @ (Expr::Dot(_, _, false) | Expr::Index(_, _, false) | Expr::SuperDot(_) | Expr::SuperIndex(_)) => Pat::Target(e),
            Expr::Array(items) => {
                let mut out = Vec::new();
                let mut rest = None;
                for i in items {
                    match i {
                        None => out.push(None),
                        Some(Expr::Spread(x)) => rest = Some(Box::new(self.to_pat(*x)?)),
                        Some(x) => out.push(Some(self.to_pat(x)?)),
                    }
                }
                Pat::Arr(out, rest)
            }
            Expr::Object(props) => {
                let mut out = Vec::new();
                let mut rest = None;
                for p in props {
                    match p.kind {
                        PropKind::Spread => rest = Some(Box::new(self.to_pat(p.value)?)),
                        PropKind::Init => out.push((p.key, self.to_pat(p.value)?)),
                        PropKind::Shorthand(d) => {
                            let id = self.to_pat(p.value)?;
                            out.push((p.key, if let Some(d) = d { Pat::Def(Box::new(id), d) } else { id }));
                        }
                        PropKind::Get | PropKind::Set => return self.err("destinazione non valida"),
                    }
                }
                Pat::Obj(out, rest)
            }
            Expr::Assign(None, p, d) => Pat::Def(p, *d),
            _ => return self.err("destinazione dell'assegnamento non valida"),
        })
    }

    // -- Expressions ------------------------------------------------------

    pub fn expr(&mut self) -> R<Expr> {
        let first = self.assign()?;
        if !self.is(",") {
            return Ok(first);
        }
        let mut list = vec![first];
        while self.eat(",") {
            list.push(self.assign()?);
        }
        Ok(Expr::Seq(list))
    }

    fn arrow_ahead(&self, k: usize) -> bool {
        let i = (self.p + k).min(self.t.len() - 1);
        matches!(self.t[i].tok, Tok::Punct("(")) && self.close[i] > 0 && matches!(self.t.get(self.close[i] + 1).map(|t| &t.tok), Some(Tok::Punct("=>")))
    }

    fn assign(&mut self) -> R<Expr> {
        self.deeper()?;
        let e = self.assign_inner();
        self.depth -= 1;
        e
    }

    fn assign_inner(&mut self) -> R<Expr> {
        match self.peek().clone() {
            Tok::Name(n) if &*n == "async" && !self.nl_at(1) => {
                if matches!(self.peek_at(1), Tok::Name(_)) && matches!(self.peek_at(2), Tok::Punct("=>")) {
                    self.next();
                    return self.arrow(true);
                }
                if self.arrow_ahead(1) {
                    self.next();
                    return self.arrow(true);
                }
            }
            Tok::Name(n) if !is_reserved(&n) && matches!(self.peek_at(1), Tok::Punct("=>")) => return self.arrow(false),
            Tok::Punct("(") if self.arrow_ahead(0) => return self.arrow(false),
            _ => {}
        }
        let lhs = self.cond()?;
        let Tok::Punct(p) = *self.peek() else { return Ok(lhs) };
        let Some(op) = assign_op(p) else { return Ok(lhs) };
        self.next();
        let pat = if op.is_none() {
            self.to_pat(lhs)?
        } else {
            match lhs {
                Expr::Ident(n) => Pat::Id(n),
                e @ (Expr::Dot(_, _, false) | Expr::Index(_, _, false) | Expr::SuperDot(_) | Expr::SuperIndex(_)) => Pat::Target(e),
                _ => return self.err("destinazione dell'assegnamento non valida"),
            }
        };
        let rhs = self.assign()?;
        Ok(Expr::Assign(op, Box::new(pat), Box::new(rhs)))
    }

    fn cond(&mut self) -> R<Expr> {
        let c = self.binary(1)?;
        if !self.eat("?") {
            return Ok(c);
        }
        let save = self.no_in;
        self.no_in = false;
        let a = self.assign();
        self.no_in = save;
        let a = a?;
        self.expect(":")?;
        let b = self.assign()?;
        Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)))
    }

    fn binary(&mut self, min: u8) -> R<Expr> {
        let mut l = self.unary()?;
        loop {
            let (op, prec) = match self.peek() {
                Tok::Punct(p) => match binary_op(p) {
                    Some(x) => x,
                    None => break,
                },
                Tok::Name(n) if &**n == "instanceof" => (Op::InstanceOf, 8),
                Tok::Name(n) if &**n == "in" && !self.no_in => (Op::In, 8),
                _ => break,
            };
            if prec < min {
                break;
            }
            self.next();
            let r = if op == Op::Exp { self.binary(prec)? } else { self.binary(prec + 1)? };
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn unary(&mut self) -> R<Expr> {
        let op = match self.peek() {
            Tok::Punct("!") => Some(Unary::Not),
            Tok::Punct("~") => Some(Unary::BitNot),
            Tok::Punct("+") => Some(Unary::Plus),
            Tok::Punct("-") => Some(Unary::Neg),
            Tok::Name(n) if &**n == "typeof" => Some(Unary::TypeOf),
            Tok::Name(n) if &**n == "void" => Some(Unary::Void),
            Tok::Name(n) if &**n == "delete" => Some(Unary::Delete),
            _ => None,
        };
        if let Some(op) = op {
            self.next();
            self.deeper()?;
            let e = self.unary();
            self.depth -= 1;
            return Ok(Expr::Unary(op, Box::new(e?)));
        }
        if self.is("++") || self.is("--") {
            let inc = self.is("++");
            self.next();
            let e = self.unary()?;
            return Ok(Expr::Update(inc, true, Box::new(e)));
        }
        if self.is_name("await") && self.fns.last().is_some_and(|c| c.is_async) {
            self.next();
            let e = self.unary()?;
            return Ok(Expr::Await(Box::new(e)));
        }
        let e = self.call_member()?;
        if (self.is("++") || self.is("--")) && !self.cur().nl {
            let inc = self.is("++");
            self.next();
            return Ok(Expr::Update(inc, false, Box::new(e)));
        }
        Ok(e)
    }

    fn args(&mut self) -> R<Vec<Expr>> {
        self.expect("(")?;
        let save = self.no_in;
        self.no_in = false;
        let mut out = Vec::new();
        let r = (|| {
            while !self.eat(")") {
                if self.eat("...") {
                    out.push(Expr::Spread(Box::new(self.assign()?)));
                } else {
                    out.push(self.assign()?);
                }
                if !self.is(")") {
                    self.expect(",")?;
                }
            }
            Ok(())
        })();
        self.no_in = save;
        r.map(|()| out)
    }

    fn index(&mut self) -> R<Expr> {
        let save = self.no_in;
        self.no_in = false;
        let e = self.expr();
        self.no_in = save;
        let e = e?;
        self.expect("]")?;
        Ok(e)
    }

    /// A name after `.`: keywords are fine there.
    fn member_name(&mut self) -> R<Name> {
        match self.next() {
            Tok::Name(n) => Ok(n),
            _ => {
                self.p -= 1;
                self.err("atteso un nome dopo '.'")
            }
        }
    }

    fn call_member(&mut self) -> R<Expr> {
        let mut e = if self.is_name("new") {
            self.new_expr()?
        } else if self.is_name("super") {
            self.next();
            if self.is("(") {
                Expr::SuperCall(self.args()?)
            } else if self.eat(".") {
                Expr::SuperDot(self.member_name()?)
            } else if self.eat("[") {
                Expr::SuperIndex(Box::new(self.index()?))
            } else {
                return self.err("'super' non valido");
            }
        } else {
            self.primary()?
        };
        let mut chain = false;
        loop {
            match self.peek().clone() {
                Tok::Punct(".") => {
                    self.next();
                    e = Expr::Dot(Box::new(e), self.member_name()?, false);
                }
                Tok::Punct("?.") => {
                    self.next();
                    chain = true;
                    e = if self.is("(") {
                        Expr::Call(Box::new(e), self.args()?, true)
                    } else if self.eat("[") {
                        Expr::Index(Box::new(e), Box::new(self.index()?), true)
                    } else {
                        Expr::Dot(Box::new(e), self.member_name()?, true)
                    };
                }
                Tok::Punct("[") => {
                    self.next();
                    e = Expr::Index(Box::new(e), Box::new(self.index()?), false);
                }
                Tok::Punct("(") => e = Expr::Call(Box::new(e), self.args()?, false),
                Tok::Tmpl(parts, srcs) => {
                    self.next();
                    let exprs = self.template_exprs(&srcs)?;
                    e = Expr::Tagged(Box::new(e), parts, exprs);
                }
                _ => break,
            }
        }
        Ok(if chain { Expr::Chain(Box::new(e)) } else { e })
    }

    fn new_expr(&mut self) -> R<Expr> {
        self.next();
        if self.eat(".") {
            let n = self.member_name()?;
            if &*n != "target" {
                return self.err("atteso 'new.target'");
            }
            return Ok(Expr::NewTarget);
        }
        let mut callee = if self.is_name("new") { self.new_expr()? } else { self.primary()? };
        loop {
            if self.eat(".") {
                callee = Expr::Dot(Box::new(callee), self.member_name()?, false);
            } else if self.eat("[") {
                callee = Expr::Index(Box::new(callee), Box::new(self.index()?), false);
            } else {
                break;
            }
        }
        let args = if self.is("(") { self.args()? } else { Vec::new() };
        Ok(Expr::New(Box::new(callee), args))
    }

    fn template_exprs(&mut self, srcs: &[String]) -> R<Vec<Expr>> {
        let is_async = self.fns.last().is_some_and(|c| c.is_async);
        let mut out = Vec::new();
        for s in srcs {
            let mut sub = Parser::new(s, is_async).map_err(|e| format!("riga {}: nel template: {e}", self.line()))?;
            sub.depth = self.depth;
            let e = sub.expr()?;
            if !matches!(sub.peek(), Tok::Eof) {
                return sub.err("template non valido");
            }
            if sub.fns[0].uses_args {
                self.mark_arguments();
            }
            out.push(e);
        }
        Ok(out)
    }

    fn primary(&mut self) -> R<Expr> {
        let line = self.line();
        let tok = self.next();
        Ok(match tok {
            Tok::Num(n) => Expr::Num(n),
            Tok::Str(s) => Expr::Str(s),
            Tok::Tmpl(parts, srcs) => {
                let exprs = self.template_exprs(&srcs)?;
                Expr::Tmpl(parts, exprs)
            }
            Tok::Regex(p, f) => Expr::Regex(p, f),
            Tok::Punct("(") => {
                let save = self.no_in;
                self.no_in = false;
                let e = self.expr();
                self.no_in = save;
                let e = e?;
                self.expect(")")?;
                e
            }
            Tok::Punct("[") => {
                let save = self.no_in;
                self.no_in = false;
                let a = self.array();
                self.no_in = save;
                a?
            }
            Tok::Punct("{") => {
                let save = self.no_in;
                self.no_in = false;
                let o = self.object();
                self.no_in = save;
                o?
            }
            Tok::Name(n) => match &*n {
                "function" => {
                    let name = if let Tok::Name(_) = self.peek() { Some(self.ident()?) } else { None };
                    Expr::Func(self.function(name, false, false)?)
                }
                "async" if self.is_name("function") && !self.cur().nl => {
                    self.next();
                    let name = if let Tok::Name(_) = self.peek() { Some(self.ident()?) } else { None };
                    Expr::Func(self.function(name, true, false)?)
                }
                "class" => {
                    self.p -= 1;
                    Expr::Class(self.class()?)
                }
                "this" => Expr::This,
                "null" => Expr::Null,
                "true" => Expr::Bool(true),
                "false" => Expr::Bool(false),
                "import" => return Err(format!("riga {line}: i moduli non sono supportati")),
                "arguments" => {
                    self.mark_arguments();
                    Expr::Ident(n)
                }
                _ if is_reserved(&n) => {
                    self.p -= 1;
                    return self.err("espressione attesa");
                }
                _ => Expr::Ident(n),
            },
            _ => {
                self.p -= 1;
                return self.err("espressione attesa");
            }
        })
    }

    fn array(&mut self) -> R<Expr> {
        let mut items = Vec::new();
        while !self.eat("]") {
            if self.eat(",") {
                items.push(None);
                continue;
            }
            if self.eat("...") {
                items.push(Some(Expr::Spread(Box::new(self.assign()?))));
            } else {
                items.push(Some(self.assign()?));
            }
            if !self.is("]") {
                self.expect(",")?;
            }
        }
        Ok(Expr::Array(items))
    }

    fn object(&mut self) -> R<Expr> {
        let mut props = Vec::new();
        while !self.eat("}") {
            if self.eat("...") {
                props.push(PropDef { key: PropName::Name(Rc::from("")), value: self.assign()?, kind: PropKind::Spread });
            } else {
                let (accessor, is_async) = self.method_prefix();
                let key = self.prop_name()?;
                if self.is("(") {
                    let fname = match &key {
                        PropName::Name(n) => Some(n.clone()),
                        PropName::Computed(_) => None,
                    };
                    let f = Expr::Func(self.function(fname, is_async, true)?);
                    let kind = match accessor {
                        Some(true) => PropKind::Get,
                        Some(false) => PropKind::Set,
                        None => PropKind::Init,
                    };
                    props.push(PropDef { key, value: f, kind });
                } else if self.eat(":") {
                    props.push(PropDef { key, value: self.assign()?, kind: PropKind::Init });
                } else {
                    let PropName::Name(n) = &key else { return self.err("atteso ':'") };
                    if &**n == "arguments" {
                        self.mark_arguments();
                    }
                    let value = Expr::Ident(n.clone());
                    let def = if self.eat("=") { Some(self.assign()?) } else { None };
                    let kind = if def.is_some() { PropKind::Shorthand(def) } else { PropKind::Init };
                    props.push(PropDef { key, value, kind });
                }
            }
            if !self.is("}") {
                self.expect(",")?;
            }
        }
        Ok(Expr::Object(props))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) -> Program {
        parse(src).unwrap_or_else(|e| panic!("{src}: {e}"))
    }

    #[test]
    fn statements() {
        ok("var a = 1, b; let [x, , ...y] = z; const {p, q: {r = 2}, ...s} = o;");
        ok("for (let i = 0; i < 3; i++) { if (i) continue; else break; }");
        ok("for (const k in o) {} for (x of xs) ; outer: while (1) { do { break outer } while (0) }");
        ok("try { throw new Error('x') } catch ({message}) {} finally {}");
        ok("switch (a) { case 1: b(); break; default: c() }");
        ok("a = 1\nb = 2\n++c");
    }

    #[test]
    fn expressions() {
        ok("x = a ? b : c ?? d || e && !f");
        ok("f = (a, {b} = {}, ...c) => a + b; g = async x => await x; h = async (y) => { return y }");
        ok("o = { a, b: 1, [k]: 2, get g() { return 1 }, set g(v) {}, m() {}, async n() {}, ...rest }");
        ok("v = a?.b?.[c]?.(d); w = new Foo.Bar(1).baz; t = tag`x${1}`; [a, b] = [b, a]");
        ok("class A extends B { #p = 1; static s = 2; constructor() { super(); } get x() { return this.#p } static { A.t = 1 } }");
        ok("x = 2 ** 3 ** 2; y = typeof z === 'undefined'; delete o[k]; void 0");
    }

    #[test]
    fn errors() {
        assert!(parse("function* g() {}").is_err());
        assert!(parse("import x from 'y'").is_err());
        assert!(parse("a = (1").is_err());
    }
}
