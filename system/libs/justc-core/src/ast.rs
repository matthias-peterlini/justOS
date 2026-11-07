//! Typed abstract syntax tree for JustScript.

use crate::prelude::*;
use crate::span::Span;

pub use crate::ui::UiKind;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Module {
    pub stmts: Vec<Stmt>,
}

impl Module {
    /// Paths written as string literals in `image "..."` elements, for
    /// checking them against a project's assets. Interpolated paths are
    /// only known at runtime and are not included.
    pub fn image_literals(&self) -> Vec<(&str, Span)> {
        fn walk<'a>(stmts: &'a [Stmt], out: &mut Vec<(&'a str, Span)>) {
            for stmt in stmts {
                match &stmt.kind {
                    StmtKind::If { branches, otherwise } => {
                        branches.iter().for_each(|(_, b)| walk(&b.stmts, out));
                        if let Some(b) = otherwise {
                            walk(&b.stmts, out);
                        }
                    }
                    StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::View(body) => walk(&body.stmts, out),
                    StmtKind::Fn(f) => walk(&f.body.stmts, out),
                    StmtKind::Struct(TypeDecl { methods, .. }) => methods.iter().for_each(|f| walk(&f.body.stmts, out)),
                    StmtKind::Match { arms, .. } => arms.iter().for_each(|a| walk(&a.body.stmts, out)),
                    StmtKind::Ui(el) => {
                        if let (UiKind::Image, Some(Expr { kind: ExprKind::Str(path), span })) = (el.kind, &el.arg) {
                            out.push((path, *span));
                        }
                        if let Some(b) = &el.children {
                            walk(&b.stmts, out);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.stmts, &mut out);
        out
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    /// `let name = value`
    Let {
        name: Ident,
        value: Expr,
    },
    /// `target = value` or `target op= value`. `target` is an identifier or an index expression.
    Assign {
        target: Expr,
        op: Option<BinaryOp>,
        value: Expr,
    },
    Expr(Expr),
    If {
        branches: Vec<(Cond, Block)>,
        otherwise: Option<Block>,
    },
    While {
        cond: Expr,
        body: Block,
    },
    For {
        var: Ident,
        iterable: Expr,
        body: Block,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    Fn(FnDecl),
    /// `struct Name` or `enum Name` with its fields or variants and methods.
    Struct(TypeDecl),
    Match {
        subject: Expr,
        arms: Vec<MatchArm>,
    },
    /// `import name` or `import name.{a, b}`.
    Import(Import),
    /// The reactive `view` declaration, re-evaluated after every UI event.
    View(Block),
    Ui(UiElement),
    /// `name: value, ...` lines inside an element block (or a component
    /// function); they apply to the innermost open element at runtime.
    Attrs(Vec<AttrInit>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    pub name: Ident,
    pub params: Vec<Ident>,
    pub body: Block,
}

/// The condition of an `if` / `elif` branch.
#[derive(Clone, Debug, PartialEq)]
pub enum Cond {
    Expr(Expr),
    /// `if let pattern = value`
    Let(Pattern, Expr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeKind {
    Struct,
    Enum,
}

/// `struct` (fields) or `enum` (variants), plus methods declared in its block.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeDecl {
    pub kind: TypeKind,
    pub name: Ident,
    pub fields: Vec<FieldDecl>,
    pub variants: Vec<VariantDecl>,
    pub methods: Vec<FnDecl>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldDecl {
    pub name: Ident,
    pub default: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariantDecl {
    pub name: Ident,
    /// Names of the variant's values (documentation only); `None` for a
    /// unit variant written without parentheses.
    pub fields: Vec<Ident>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PatternKind {
    /// `_`
    Wildcard,
    /// A name that captures the value.
    Bind(Ident),
    /// A literal compared with `==` (int, float, string, bool or nil).
    Literal(Expr),
    /// `lo..hi`: an int with `lo <= x < hi`.
    Range(Expr, Expr),
    /// `Name`, `Name(p, ...)`, `Type.Name` or `Type.Name(p, ...)`: an enum
    /// variant, or a struct destructured by field position. Bare names that
    /// are not variants are bindings, which the compiler decides.
    Ctor { path: Vec<Ident>, args: Option<Vec<Pattern>> },
    /// `a | b | c`
    Or(Vec<Pattern>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Import {
    /// The module name: `utils` for `utils.jt` next to the importing file.
    pub module: Ident,
    /// Names imported directly (`import utils.{a, b}`); empty means the
    /// module is used qualified (`utils.a`).
    pub names: Vec<Ident>,
}

/// A declarative UI element such as `button "Ok" style: "filled" -> submit()`.
#[derive(Clone, Debug, PartialEq)]
pub struct UiElement {
    pub kind: UiKind,
    /// The element keyword.
    pub kind_span: Span,
    pub arg: Option<Expr>,
    /// Attributes written on the element line.
    pub attrs: Vec<AttrInit>,
    /// Event handler after `->`.
    pub action: Option<Expr>,
    /// The indented block: attribute lines, and child elements for containers.
    pub children: Option<Block>,
}

/// `name: value`
#[derive(Clone, Debug, PartialEq)]
pub struct AttrInit {
    pub name: Ident,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Nil,
    Interp(Vec<InterpPart>),
    Ident(String),
    List(Vec<Expr>),
    /// `{key: value, ...}`
    Map(Vec<(Expr, Expr)>),
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Logical {
        op: LogicalOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `f(a, b)`; `named` holds `name: value` arguments (struct construction).
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        named: Vec<(Ident, Expr)>,
    },
    /// `x.f(a)`: a method of `x`'s type, or the function `f(x, a)`; also
    /// `Type.f(a)`, `Type.Variant(a)` and `module.f(a)`.
    MethodCall {
        receiver: Box<Expr>,
        method: Ident,
        args: Vec<Expr>,
        named: Vec<(Ident, Expr)>,
    },
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
    },
    /// `x.name`: a struct field, or `Type.name` (variant or associated function).
    Field {
        target: Box<Expr>,
        name: Ident,
    },
    /// `|a, b| expr`
    Lambda {
        params: Vec<Ident>,
        body: Box<Expr>,
    },
    /// `expr?`
    Try(Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum InterpPart {
    Text(String),
    Expr(Expr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Range,
}

impl BinaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Rem => "%",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::Range => "..",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicalOp {
    And,
    Or,
}
