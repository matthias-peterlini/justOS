//! The syntax tree the parser builds and the interpreter walks.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

pub type Name = Rc<str>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Gt,
    Le,
    Ge,
    In,
    InstanceOf,
    And,
    Or,
    Nullish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unary {
    Neg,
    Plus,
    Not,
    BitNot,
    TypeOf,
    Void,
    Delete,
}

#[derive(Debug)]
pub enum Expr {
    Num(f64),
    Str(Name),
    /// Pieces and the expressions between them.
    Tmpl(Vec<Name>, Vec<Expr>),
    Tagged(Box<Expr>, Vec<Name>, Vec<Expr>),
    Regex(Name, Name),
    Ident(Name),
    This,
    Null,
    Bool(bool),
    /// `None` is a hole.
    Array(Vec<Option<Expr>>),
    Object(Vec<PropDef>),
    Func(Rc<FuncDef>),
    Class(Rc<ClassDef>),
    Unary(Unary, Box<Expr>),
    /// Increment?, prefix?, target.
    Update(bool, bool, Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
    /// `None` for plain `=`.
    Assign(Option<Op>, Box<Pat>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    /// Callee, arguments, `?.(`.
    Call(Box<Expr>, Vec<Expr>, bool),
    New(Box<Expr>, Vec<Expr>),
    /// Object, name, `?.`.
    Dot(Box<Expr>, Name, bool),
    /// Object, key, `?.`.
    Index(Box<Expr>, Box<Expr>, bool),
    Seq(Vec<Expr>),
    Spread(Box<Expr>),
    /// Where a `?.` stops the rest of a chain.
    Chain(Box<Expr>),
    Await(Box<Expr>),
    SuperCall(Vec<Expr>),
    SuperDot(Name),
    SuperIndex(Box<Expr>),
    NewTarget,
}

#[derive(Debug)]
pub enum PropName {
    Name(Name),
    Computed(Expr),
}

#[derive(Debug)]
pub enum PropKind {
    Init,
    Get,
    Set,
    Spread,
    /// `{ a = 1 }`, only valid as a pattern.
    Shorthand(Option<Expr>),
}

#[derive(Debug)]
pub struct PropDef {
    pub key: PropName,
    pub value: Expr,
    pub kind: PropKind,
}

#[derive(Debug)]
pub enum Pat {
    Id(Name),
    /// A member to assign to (`[a.b] = …`).
    Target(Expr),
    Arr(Vec<Option<Pat>>, Option<Box<Pat>>),
    Obj(Vec<(PropName, Pat)>, Option<Box<Pat>>),
    /// A pattern with a default value.
    Def(Box<Pat>, Expr),
}

#[derive(Debug)]
pub enum Body {
    Block(Vec<Stmt>),
    Expr(Expr),
}

#[derive(Debug)]
pub struct FuncDef {
    pub name: Option<Name>,
    pub params: Vec<Pat>,
    pub rest: Option<Pat>,
    pub body: Body,
    pub arrow: bool,
    pub is_async: bool,
    /// Methods and accessors can't be constructed.
    pub method: bool,
    /// Every `var` in the body (not in nested functions).
    pub vars: Vec<Name>,
    pub uses_args: bool,
    pub line: u32,
}

#[derive(Debug)]
pub enum MemberKind {
    Method(Rc<FuncDef>),
    Get(Rc<FuncDef>),
    Set(Rc<FuncDef>),
    Field(Option<Expr>),
    /// `static { … }`.
    Block(Rc<FuncDef>),
}

#[derive(Debug)]
pub struct ClassMember {
    pub key: PropName,
    pub is_static: bool,
    pub kind: MemberKind,
}

#[derive(Debug)]
pub struct ClassDef {
    pub name: Option<Name>,
    pub extends: Option<Expr>,
    pub ctor: Rc<FuncDef>,
    pub members: Vec<ClassMember>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclKind {
    Var,
    Let,
    Const,
}

#[derive(Debug)]
pub enum Stmt {
    Expr(Expr),
    Decl(DeclKind, Vec<(Pat, Option<Expr>)>),
    Func(Rc<FuncDef>),
    Class(Rc<ClassDef>),
    Return(Option<Expr>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    /// Statements, and whether they declare anything block-scoped.
    Block(Vec<Stmt>, bool),
    For(Option<Box<Stmt>>, Option<Expr>, Option<Expr>, Box<Stmt>),
    ForIn(Option<DeclKind>, Pat, Expr, Box<Stmt>),
    ForOf(Option<DeclKind>, Pat, Expr, Box<Stmt>),
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    Break(Option<Name>),
    Continue(Option<Name>),
    Throw(Expr),
    Try(Vec<Stmt>, Option<(Option<Pat>, Vec<Stmt>)>, Option<Vec<Stmt>>),
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>),
    Label(Name, Box<Stmt>),
    Empty,
}

impl Stmt {
    /// Whether the statement declares something for its block.
    pub fn lexical(&self) -> bool {
        matches!(self, Stmt::Decl(DeclKind::Let | DeclKind::Const, _) | Stmt::Func(_) | Stmt::Class(_))
    }

    pub fn is_loop(&self) -> bool {
        matches!(self, Stmt::For(..) | Stmt::ForIn(..) | Stmt::ForOf(..) | Stmt::While(..) | Stmt::DoWhile(..))
    }
}

/// A parsed script.
#[derive(Debug)]
pub struct Program {
    pub body: Vec<Stmt>,
    pub vars: Vec<Name>,
}
