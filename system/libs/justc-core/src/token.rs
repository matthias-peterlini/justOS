//! Token definitions produced by the [`lexer`](crate::lexer).

use crate::prelude::*;
use crate::span::Span;
use core::fmt;

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// A piece of a string literal: literal text or an interpolated expression.
#[derive(Clone, Debug, PartialEq)]
pub enum StrSegment {
    Text(String),
    /// Tokens of `{ ... }`, terminated by [`TokenKind::Eof`]. `span` covers the
    /// expression text between the braces.
    Expr {
        tokens: Vec<Token>,
        span: Span,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    // Literals & names.
    Int(i64),
    Float(f64),
    Str(Vec<StrSegment>),
    Ident(String),

    // Keywords.
    Let,
    Fn,
    Return,
    If,
    Elif,
    Else,
    While,
    For,
    In,
    Break,
    Continue,
    True,
    False,
    Nil,
    And,
    Or,
    Not,
    View,
    Struct,
    Enum,
    Match,
    Import,

    // Punctuation & operators.
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Dot,
    DotDot,
    Colon,
    Arrow,
    /// `=>` in match arms.
    FatArrow,
    /// `|` in lambdas and or-patterns.
    Pipe,
    /// `?` error propagation.
    Question,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Assign,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,

    // Layout.
    Newline,
    Indent,
    Dedent,
    Eof,
}

impl TokenKind {
    pub fn keyword(ident: &str) -> Option<TokenKind> {
        Some(match ident {
            "let" => TokenKind::Let,
            "fn" => TokenKind::Fn,
            "return" => TokenKind::Return,
            "if" => TokenKind::If,
            "elif" => TokenKind::Elif,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "nil" => TokenKind::Nil,
            "and" => TokenKind::And,
            "or" => TokenKind::Or,
            "not" => TokenKind::Not,
            "view" => TokenKind::View,
            "struct" => TokenKind::Struct,
            "enum" => TokenKind::Enum,
            "match" => TokenKind::Match,
            "import" => TokenKind::Import,
            _ => return None,
        })
    }

    pub fn is_keyword(&self) -> bool {
        use TokenKind::*;
        matches!(
            self,
            Let | Fn
                | Return
                | If
                | Elif
                | Else
                | While
                | For
                | In
                | Break
                | Continue
                | True
                | False
                | Nil
                | And
                | Or
                | Not
                | View
                | Struct
                | Enum
                | Match
                | Import
        )
    }
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use TokenKind::*;
        let s = match self {
            Int(v) => return write!(f, "integer `{v}`"),
            Float(v) => return write!(f, "float `{v}`"),
            Str(_) => "string literal",
            Ident(name) => return write!(f, "identifier `{name}`"),
            Let => "`let`",
            Fn => "`fn`",
            Return => "`return`",
            If => "`if`",
            Elif => "`elif`",
            Else => "`else`",
            While => "`while`",
            For => "`for`",
            In => "`in`",
            Break => "`break`",
            Continue => "`continue`",
            True => "`true`",
            False => "`false`",
            Nil => "`nil`",
            And => "`and`",
            Or => "`or`",
            Not => "`not`",
            View => "`view`",
            Struct => "`struct`",
            Enum => "`enum`",
            Match => "`match`",
            Import => "`import`",
            LParen => "`(`",
            RParen => "`)`",
            LBracket => "`[`",
            RBracket => "`]`",
            LBrace => "`{`",
            RBrace => "`}`",
            Comma => "`,`",
            Dot => "`.`",
            DotDot => "`..`",
            Colon => "`:`",
            Arrow => "`->`",
            FatArrow => "`=>`",
            Pipe => "`|`",
            Question => "`?`",
            Plus => "`+`",
            Minus => "`-`",
            Star => "`*`",
            Slash => "`/`",
            Percent => "`%`",
            Assign => "`=`",
            PlusAssign => "`+=`",
            MinusAssign => "`-=`",
            StarAssign => "`*=`",
            SlashAssign => "`/=`",
            EqEq => "`==`",
            NotEq => "`!=`",
            Lt => "`<`",
            Le => "`<=`",
            Gt => "`>`",
            Ge => "`>=`",
            Newline => "end of line",
            Indent => "indentation",
            Dedent => "end of block",
            Eof => "end of file",
        };
        f.write_str(s)
    }
}
