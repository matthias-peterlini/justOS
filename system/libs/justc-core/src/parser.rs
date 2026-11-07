//! Recursive-descent parser producing a typed [`ast::Module`](crate::ast::Module).
//!
//! Expression precedence, lowest to highest:
//!
//! | level | operators                         |
//! |-------|-----------------------------------|
//! | 1     | `or`                              |
//! | 2     | `and`                             |
//! | 3     | `not` (prefix)                    |
//! | 4     | `== != < <= > >=` (non-chaining)  |
//! | 5     | `..`                              |
//! | 6     | `+ -`                             |
//! | 7     | `* / %`                           |
//! | 8     | `-` (prefix)                      |
//! | 9     | call `f()`, index `a[i]`, field `a.x`, method `a.f()`, `a?` |
//!
//! A lambda `|a, b| body` extends as far to the right as possible.
//!
//! Errors never abort parsing: a diagnostic is recorded and the parser
//! re-synchronizes at the next statement boundary.

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::lexer;
use crate::prelude::*;
use crate::span::Span;
use crate::token::{StrSegment, Token, TokenKind};
use crate::ui::{Arg, Event};

#[derive(Debug, Default)]
pub struct ParseOutput {
    pub module: Module,
    pub diagnostics: Vec<Diagnostic>,
}

/// Lex and parse a complete source file.
pub fn parse(src: &str) -> ParseOutput {
    parse_at(src, 0)
}

/// Parse a file whose spans start at `base` (see [`lexer::tokenize_at`]).
pub fn parse_at(src: &str, base: u32) -> ParseOutput {
    let lexed = lexer::tokenize_at(src, base);
    let mut parser = Parser::new(&lexed.tokens);
    let module = parser.parse_module();
    let mut diagnostics = lexed.diagnostics;
    diagnostics.append(&mut parser.diagnostics);
    diagnostics.sort_by_key(|d| d.span.start);
    ParseOutput { module, diagnostics }
}

/// Marker for "a diagnostic was already recorded".
#[derive(Debug)]
struct Reported;

type PResult<T> = Result<T, Reported>;

/// Positional and `name: value` call arguments.
type CallArgs = (Vec<Expr>, Vec<(Ident, Expr)>);

struct Parser<'t> {
    tokens: &'t [Token],
    pos: usize,
    diagnostics: Vec<Diagnostic>,
    loop_depth: u32,
    block_depth: u32,
}

impl<'t> Parser<'t> {
    fn new(tokens: &'t [Token]) -> Self {
        debug_assert!(matches!(tokens.last(), Some(Token { kind: TokenKind::Eof, .. })));
        Parser { tokens, pos: 0, diagnostics: Vec::new(), loop_depth: 0, block_depth: 0 }
    }

    // ------------------------------------------------------------ token cursor

    #[inline]
    fn peek(&self) -> &'t Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    #[inline]
    fn peek_kind(&self) -> &'t TokenKind {
        &self.peek().kind
    }

    fn peek_nth(&self, n: usize) -> &'t TokenKind {
        &self.tokens[(self.pos + n).min(self.tokens.len() - 1)].kind
    }

    #[inline]
    fn check(&self, kind: &TokenKind) -> bool {
        core::mem::discriminant(self.peek_kind()) == core::mem::discriminant(kind)
    }

    fn bump(&mut self) -> &'t Token {
        let tok = self.peek();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        tok
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.check(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn prev_span(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1)].span
    }

    fn expect(&mut self, kind: &TokenKind, what: &str) -> PResult<&'t Token> {
        if self.check(kind) {
            Ok(self.bump())
        } else {
            Err(self.error_here(format!("expected {what}, found {}", self.peek_kind())))
        }
    }

    fn expect_ident(&mut self, what: &str) -> PResult<Ident> {
        let tok = self.peek();
        match &tok.kind {
            TokenKind::Ident(name) => {
                self.bump();
                Ok(Ident { name: name.clone(), span: tok.span })
            }
            other if other.is_keyword() => {
                let d = Diagnostic::error(format!("expected {what}, found keyword {other}"), tok.span)
                    .with_note("keywords cannot be used as names");
                Err(self.report(d))
            }
            other => Err(self.error_here(format!("expected {what}, found {other}"))),
        }
    }

    fn report(&mut self, d: Diagnostic) -> Reported {
        self.diagnostics.push(d);
        Reported
    }

    fn error_here(&mut self, msg: String) -> Reported {
        let span = self.peek().span;
        self.report(Diagnostic::error(msg, span))
    }

    /// Skip to the start of the next statement, swallowing any block that
    /// belongs to the broken statement.
    fn synchronize(&mut self) {
        let mut depth = 0u32;
        loop {
            match self.peek_kind() {
                TokenKind::Eof => return,
                TokenKind::Newline => {
                    self.bump();
                    if depth == 0 && !self.check(&TokenKind::Indent) {
                        return;
                    }
                }
                TokenKind::Indent => {
                    depth += 1;
                    self.bump();
                }
                TokenKind::Dedent => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                    self.bump();
                    if depth == 0 {
                        return;
                    }
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    // -------------------------------------------------------------- statements

    fn parse_module(&mut self) -> Module {
        let mut stmts = Vec::new();
        while !self.check(&TokenKind::Eof) {
            if self.eat(&TokenKind::Newline) {
                continue;
            }
            if self.check(&TokenKind::Indent) {
                let d =
                    Diagnostic::error("unexpected indentation", self.peek().span).with_note("top-level statements must start at column 1");
                self.report(d);
                self.bump();
                continue;
            }
            if self.check(&TokenKind::Dedent) {
                self.bump();
                continue;
            }
            match self.parse_stmt() {
                Ok(s) => stmts.push(s),
                Err(Reported) => self.synchronize(),
            }
        }
        Module { stmts }
    }

    fn parse_block(&mut self) -> PResult<Block> {
        self.eat(&TokenKind::Colon);
        let start = self.peek().span;
        if !self.check(&TokenKind::Newline) {
            return Err(self.error_here(format!("expected end of line before block, found {}", self.peek_kind())));
        }
        self.bump();
        if !self.eat(&TokenKind::Indent) {
            let d = Diagnostic::error("expected an indented block", self.peek().span)
                .with_note("indent the body of the block (4 spaces recommended)");
            return Err(self.report(d));
        }
        self.block_depth += 1;
        let mut stmts = Vec::new();
        while !self.check(&TokenKind::Dedent) && !self.check(&TokenKind::Eof) {
            if self.eat(&TokenKind::Newline) {
                continue;
            }
            match self.parse_stmt() {
                Ok(s) => stmts.push(s),
                Err(Reported) => self.synchronize(),
            }
        }
        self.block_depth -= 1;
        let end = self.prev_span();
        self.eat(&TokenKind::Dedent);
        Ok(Block { stmts, span: start.to(end) })
    }

    /// Simple statements end at a newline (or the end of the enclosing block).
    fn expect_stmt_end(&mut self) -> PResult<()> {
        match self.peek_kind() {
            TokenKind::Newline => {
                self.bump();
                Ok(())
            }
            TokenKind::Dedent | TokenKind::Eof => Ok(()),
            other => Err(self.error_here(format!("expected end of line, found {other}"))),
        }
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        let start = self.peek().span;
        let kind = match self.peek_kind() {
            TokenKind::Let => {
                self.bump();
                let name = self.expect_ident("a variable name")?;
                self.expect(&TokenKind::Assign, "`=` after variable name")?;
                let value = self.parse_expr()?;
                self.expect_stmt_end()?;
                StmtKind::Let { name, value }
            }
            TokenKind::Fn => StmtKind::Fn(self.parse_fn()?),
            TokenKind::Struct | TokenKind::Enum => {
                let tok = self.bump();
                if self.block_depth > 0 {
                    self.report(Diagnostic::error(format!("{} must be declared at the top level", tok.kind), tok.span));
                }
                let kind = if tok.kind == TokenKind::Struct { TypeKind::Struct } else { TypeKind::Enum };
                StmtKind::Struct(self.parse_type_decl(kind)?)
            }
            TokenKind::Match => self.parse_match()?,
            TokenKind::Import => {
                let tok = self.bump();
                if self.block_depth > 0 {
                    self.report(Diagnostic::error("`import` must be at the top level", tok.span));
                }
                let module = self.expect_ident("a module name after `import`")?;
                let mut names = Vec::new();
                if self.eat(&TokenKind::Dot) {
                    self.expect(&TokenKind::LBrace, "`{` after `.` (e.g. `import utils.{a, b}`)")?;
                    while !self.check(&TokenKind::RBrace) {
                        names.push(self.expect_ident("a name to import")?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(&TokenKind::RBrace, "`}` to close the import list")?;
                }
                self.expect_stmt_end()?;
                StmtKind::Import(Import { module, names })
            }
            TokenKind::Return => {
                self.bump();
                let value = if matches!(self.peek_kind(), TokenKind::Newline | TokenKind::Dedent | TokenKind::Eof) {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.expect_stmt_end()?;
                StmtKind::Return(value)
            }
            TokenKind::Break | TokenKind::Continue => {
                let tok = self.bump();
                if self.loop_depth == 0 {
                    self.report(Diagnostic::error(format!("{} outside of a loop", tok.kind), tok.span));
                }
                self.expect_stmt_end()?;
                if tok.kind == TokenKind::Break {
                    StmtKind::Break
                } else {
                    StmtKind::Continue
                }
            }
            TokenKind::If => self.parse_if()?,
            TokenKind::While => {
                self.bump();
                let cond = self.parse_expr()?;
                let body = self.parse_loop_body()?;
                StmtKind::While { cond, body }
            }
            TokenKind::For => {
                self.bump();
                let var = self.expect_ident("a loop variable")?;
                self.expect(&TokenKind::In, "`in` after loop variable")?;
                let iterable = self.parse_expr()?;
                let body = self.parse_loop_body()?;
                StmtKind::For { var, iterable, body }
            }
            TokenKind::View => {
                let tok = self.bump();
                if self.block_depth > 0 {
                    self.report(Diagnostic::error("`view` must be declared at the top level", tok.span));
                }
                StmtKind::View(self.parse_block()?)
            }
            TokenKind::Ident(name) if UiKind::from_name(name).is_some() && starts_element(self.peek_nth(1), self.peek_nth(2)) => {
                StmtKind::Ui(self.parse_ui()?)
            }
            TokenKind::Ident(_) if self.at_attr_start() => {
                let attrs = self.parse_attrs()?;
                self.expect_stmt_end()?;
                StmtKind::Attrs(attrs)
            }
            _ => {
                let target = self.parse_expr()?;
                let op = match self.peek_kind() {
                    TokenKind::Assign => Some(None),
                    TokenKind::PlusAssign => Some(Some(BinaryOp::Add)),
                    TokenKind::MinusAssign => Some(Some(BinaryOp::Sub)),
                    TokenKind::StarAssign => Some(Some(BinaryOp::Mul)),
                    TokenKind::SlashAssign => Some(Some(BinaryOp::Div)),
                    _ => None,
                };
                if let Some(op) = op {
                    let op_tok = self.bump();
                    if !matches!(target.kind, ExprKind::Ident(_) | ExprKind::Index { .. } | ExprKind::Field { .. }) {
                        self.report(
                            Diagnostic::error("invalid assignment target", target.span)
                                .with_label("cannot assign to this expression")
                                .with_note(format!("only variables, indexed elements and fields can appear left of {}", op_tok.kind)),
                        );
                    }
                    let value = self.parse_expr()?;
                    self.expect_stmt_end()?;
                    StmtKind::Assign { target, op, value }
                } else {
                    self.expect_stmt_end()?;
                    StmtKind::Expr(target)
                }
            }
        };
        Ok(Stmt { kind, span: start.to(self.prev_span()) })
    }

    fn parse_loop_body(&mut self) -> PResult<Block> {
        self.loop_depth += 1;
        let body = self.parse_block();
        self.loop_depth -= 1;
        body
    }

    fn parse_fn(&mut self) -> PResult<FnDecl> {
        self.bump(); // `fn`
        let name = self.expect_ident("a function name")?;
        self.expect(&TokenKind::LParen, "`(` after function name")?;
        let mut params: Vec<Ident> = Vec::new();
        while !self.check(&TokenKind::RParen) {
            let p = self.expect_ident("a parameter name")?;
            if params.iter().any(|q| q.name == p.name) {
                self.report(Diagnostic::error(format!("duplicate parameter `{}`", p.name), p.span));
            }
            params.push(p);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`)` to close the parameter list")?;
        if params.len() > u8::MAX as usize {
            self.report(Diagnostic::error("too many parameters (max 255)", name.span));
        }
        // A function body resets loop context: `break` cannot escape it.
        let saved_loops = core::mem::replace(&mut self.loop_depth, 0);
        self.block_depth += 1;
        let body = self.parse_block();
        self.block_depth -= 1;
        self.loop_depth = saved_loops;
        Ok(FnDecl { name, params, body: body? })
    }

    /// The block of a `struct` (fields, `name = default`) or an `enum`
    /// (variants, `Name(a, b)`), followed by methods.
    fn parse_type_decl(&mut self, kind: TypeKind) -> PResult<TypeDecl> {
        let what = if kind == TypeKind::Struct { "a struct name" } else { "an enum name" };
        let name = self.expect_ident(what)?;
        let mut decl = TypeDecl { kind, name, fields: Vec::new(), variants: Vec::new(), methods: Vec::new() };
        self.eat(&TokenKind::Colon);
        if !self.eat(&TokenKind::Newline) || !self.eat(&TokenKind::Indent) {
            let item = if kind == TypeKind::Struct { "fields" } else { "variants" };
            let d = Diagnostic::error(format!("expected an indented block of {item}"), self.peek().span).with_note(
                if kind == TypeKind::Struct {
                    "for example:\n    struct Point\n        x\n        y = 0"
                } else {
                    "for example:\n    enum Shape\n        Circle(radius)\n        Empty"
                },
            );
            return Err(self.report(d));
        }
        while !self.check(&TokenKind::Dedent) && !self.check(&TokenKind::Eof) {
            if self.eat(&TokenKind::Newline) {
                continue;
            }
            if self.check(&TokenKind::Fn) {
                match self.parse_fn() {
                    Ok(f) => decl.methods.push(f),
                    Err(Reported) => self.synchronize(),
                }
                continue;
            }
            if let Err(Reported) = self.parse_type_items(&mut decl) {
                self.synchronize();
            }
        }
        self.eat(&TokenKind::Dedent);
        let empty = if kind == TypeKind::Struct { decl.fields.is_empty() } else { decl.variants.is_empty() };
        if empty {
            let d = Diagnostic::error(
                format!("`{}` has no {}", decl.name.name, if kind == TypeKind::Struct { "fields" } else { "variants" }),
                decl.name.span,
            );
            self.report(d);
        }
        Ok(decl)
    }

    /// One line of fields or variants, comma separated.
    fn parse_type_items(&mut self, decl: &mut TypeDecl) -> PResult<()> {
        if !decl.methods.is_empty() {
            let what = if decl.kind == TypeKind::Struct { "fields" } else { "variants" };
            return Err(self.error_here(format!("{what} must come before the methods")));
        }
        loop {
            let name = self.expect_ident(if decl.kind == TypeKind::Struct { "a field name" } else { "a variant name" })?;
            match decl.kind {
                TypeKind::Struct => {
                    let default = if self.eat(&TokenKind::Assign) { Some(self.parse_expr()?) } else { None };
                    if decl.fields.iter().any(|f| f.name.name == name.name) {
                        self.report(Diagnostic::error(format!("duplicate field `{}`", name.name), name.span));
                    }
                    decl.fields.push(FieldDecl { name, default });
                }
                TypeKind::Enum => {
                    let mut fields = Vec::new();
                    if self.eat(&TokenKind::LParen) {
                        while !self.check(&TokenKind::RParen) {
                            fields.push(self.expect_ident("a name for the variant's value")?);
                            if !self.eat(&TokenKind::Comma) {
                                break;
                            }
                        }
                        self.expect(&TokenKind::RParen, "`)` to close the variant")?;
                    }
                    if decl.variants.iter().any(|v| v.name.name == name.name) {
                        self.report(Diagnostic::error(format!("duplicate variant `{}`", name.name), name.span));
                    }
                    decl.variants.push(VariantDecl { name, fields });
                }
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            if matches!(self.peek_kind(), TokenKind::Newline) {
                break;
            }
        }
        self.expect_stmt_end()
    }

    /// `match subject` followed by arms: `pattern [if guard] => statement`
    /// or `pattern [if guard]` with an indented block.
    fn parse_match(&mut self) -> PResult<StmtKind> {
        let tok = self.bump();
        let subject = self.parse_expr()?;
        self.eat(&TokenKind::Colon);
        if !self.eat(&TokenKind::Newline) || !self.eat(&TokenKind::Indent) {
            let d = Diagnostic::error("expected an indented block of match arms", self.peek().span)
                .with_note("for example:\n    match value\n        0 => print(\"zero\")\n        _ => print(\"other\")");
            return Err(self.report(d));
        }
        let mut arms = Vec::new();
        while !self.check(&TokenKind::Dedent) && !self.check(&TokenKind::Eof) {
            if self.eat(&TokenKind::Newline) {
                continue;
            }
            match self.parse_arm() {
                Ok(arm) => arms.push(arm),
                Err(Reported) => self.synchronize(),
            }
        }
        self.eat(&TokenKind::Dedent);
        if arms.is_empty() {
            self.report(Diagnostic::error("`match` has no arms", tok.span));
        }
        Ok(StmtKind::Match { subject, arms })
    }

    fn parse_arm(&mut self) -> PResult<MatchArm> {
        let pattern = self.parse_pattern()?;
        let guard = if self.eat(&TokenKind::If) { Some(self.parse_expr()?) } else { None };
        let body = if self.eat(&TokenKind::FatArrow) {
            if self.check(&TokenKind::Newline) && matches!(self.peek_nth(1), TokenKind::Indent) {
                self.parse_block()?
            } else {
                let stmt = self.parse_stmt()?;
                Block { span: stmt.span, stmts: vec![stmt] }
            }
        } else if self.check(&TokenKind::Newline) || self.check(&TokenKind::Colon) {
            self.parse_block()?
        } else {
            return Err(self.error_here(format!("expected `=>` or a block after the pattern, found {}", self.peek_kind())));
        };
        Ok(MatchArm { pattern, guard, body })
    }

    /// `alt | alt | ...`
    fn parse_pattern(&mut self) -> PResult<Pattern> {
        let first = self.parse_pattern_alt()?;
        if !self.check(&TokenKind::Pipe) {
            return Ok(first);
        }
        let mut alts = vec![first];
        while self.eat(&TokenKind::Pipe) {
            alts.push(self.parse_pattern_alt()?);
        }
        let span = alts[0].span.to(alts.last().unwrap().span);
        Ok(Pattern { kind: PatternKind::Or(alts), span })
    }

    fn parse_pattern_alt(&mut self) -> PResult<Pattern> {
        let tok = self.peek();
        match &tok.kind {
            TokenKind::Ident(name) if name == "_" => {
                self.bump();
                Ok(Pattern { kind: PatternKind::Wildcard, span: tok.span })
            }
            TokenKind::Ident(_) => {
                let mut path = vec![self.expect_ident("a pattern")?];
                while self.eat(&TokenKind::Dot) {
                    path.push(self.expect_ident("a name after `.`")?);
                }
                let args = if self.eat(&TokenKind::LParen) {
                    let mut args = Vec::new();
                    while !self.check(&TokenKind::RParen) {
                        args.push(self.parse_pattern()?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(&TokenKind::RParen, "`)` to close the pattern")?;
                    Some(args)
                } else {
                    None
                };
                Ok(Pattern { kind: PatternKind::Ctor { path, args }, span: tok.span.to(self.prev_span()) })
            }
            TokenKind::Int(_)
            | TokenKind::Float(_)
            | TokenKind::Minus
            | TokenKind::Str(_)
            | TokenKind::True
            | TokenKind::False
            | TokenKind::Nil => {
                let lo = self.parse_pattern_literal()?;
                if self.eat(&TokenKind::DotDot) {
                    let hi = self.parse_pattern_literal()?;
                    let span = lo.span.to(hi.span);
                    return Ok(Pattern { kind: PatternKind::Range(lo, hi), span });
                }
                Ok(Pattern { span: lo.span, kind: PatternKind::Literal(lo) })
            }
            other => {
                let d = Diagnostic::error(format!("expected a pattern, found {other}"), tok.span)
                    .with_note("patterns are literals (`0`, `\"x\"`), ranges (`1..10`), names, `_` or variants such as `Some(x)`");
                Err(self.report(d))
            }
        }
    }

    fn parse_pattern_literal(&mut self) -> PResult<Expr> {
        let expr = self.parse_unary()?;
        match expr.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Nil => Ok(expr),
            _ => {
                let d = Diagnostic::error("patterns can only contain literal values", expr.span)
                    .with_note("compare computed values in a guard: `x if x == limit => ...`");
                Err(self.report(d))
            }
        }
    }

    /// `expr`, or `let pattern = expr` (in `if let` / `elif let`).
    fn parse_cond(&mut self) -> PResult<Cond> {
        if self.eat(&TokenKind::Let) {
            let pattern = self.parse_pattern()?;
            self.expect(&TokenKind::Assign, "`=` after the pattern of `if let`")?;
            return Ok(Cond::Let(pattern, self.parse_expr()?));
        }
        Ok(Cond::Expr(self.parse_expr()?))
    }

    fn parse_if(&mut self) -> PResult<StmtKind> {
        self.bump(); // `if`
        let mut branches = Vec::new();
        let cond = self.parse_cond()?;
        branches.push((cond, self.parse_block()?));
        let mut otherwise = None;
        loop {
            if self.eat(&TokenKind::Elif) {
                let cond = self.parse_cond()?;
                branches.push((cond, self.parse_block()?));
            } else if self.eat(&TokenKind::Else) {
                // `else if` is accepted as an alias for `elif`.
                if self.eat(&TokenKind::If) {
                    let cond = self.parse_cond()?;
                    branches.push((cond, self.parse_block()?));
                    continue;
                }
                otherwise = Some(self.parse_block()?);
                break;
            } else {
                break;
            }
        }
        Ok(StmtKind::If { branches, otherwise })
    }

    /// `element [arg] [name: value, ...] [-> handler] [block]`
    fn parse_ui(&mut self) -> PResult<UiElement> {
        let tok = self.bump();
        let kind = match &tok.kind {
            TokenKind::Ident(name) => UiKind::from_name(name).expect("caller checked the element name"),
            _ => unreachable!("caller checked for an identifier"),
        };
        let at_colon_eol = self.check(&TokenKind::Colon) && matches!(self.peek_nth(1), TokenKind::Newline);
        let has_arg = !self.at_attr_start()
            && !at_colon_eol
            && !matches!(self.peek_kind(), TokenKind::Newline | TokenKind::Arrow | TokenKind::Dedent | TokenKind::Eof);
        let arg = if has_arg { Some(self.parse_expr()?) } else { None };
        let mut attrs = if self.at_attr_start() { self.parse_attrs()? } else { Vec::new() };
        let action = if self.check(&TokenKind::Arrow) {
            let arrow = self.bump();
            if kind.event() == Event::None {
                self.report(Diagnostic::error(format!("`{}` does not accept an event handler", kind.name()), arrow.span));
            }
            Some(self.parse_expr()?)
        } else {
            None
        };
        if self.at_attr_start() {
            let d = Diagnostic::error("attributes must come before the event handler", self.peek().span).with_note(format!(
                "write `{} ... {}: ... -> handler()`",
                kind.name(),
                self.peek_attr_name()
            ));
            self.report(d);
            attrs.extend(self.parse_attrs()?);
        }
        let opens_block = self.at_block_start() || (self.check(&TokenKind::Newline) && matches!(self.peek_nth(1), TokenKind::Indent));
        let children = if opens_block {
            Some(self.parse_block()?)
        } else {
            if self.check(&TokenKind::Colon) && matches!(self.peek_nth(1), TokenKind::Newline) {
                self.bump();
            }
            self.expect_stmt_end()?;
            None
        };
        match (kind.arg(), &arg) {
            (Arg::Required(ty), None) => {
                self.report(
                    Diagnostic::error(format!("`{}` requires an argument ({})", kind.name(), ty.describe()), tok.span)
                        .with_note(format!("for example: {}", element_example(kind))),
                );
            }
            (Arg::None, Some(a)) => {
                let d = Diagnostic::error(format!("`{}` does not take an argument", kind.name()), a.span)
                    .with_label("unexpected argument")
                    .with_note("attributes are written as `name: value`, e.g. `padding: 16`");
                self.report(d);
            }
            _ => {}
        }
        if kind.event() == Event::Click && action.is_none() {
            self.diagnostics.push(
                Diagnostic::warning(format!("{} has no event handler", kind.name()), tok.span).with_note("add one with `-> handler()`"),
            );
        }
        Ok(UiElement { kind, kind_span: tok.span, arg, attrs, action, children })
    }

    /// `name: value, name: value` (a trailing comma is allowed).
    fn parse_attrs(&mut self) -> PResult<Vec<AttrInit>> {
        let mut attrs = Vec::new();
        loop {
            let name = self.expect_ident("an attribute name")?;
            self.expect(&TokenKind::Colon, "`:` after the attribute name")?;
            let value = self.parse_expr()?;
            attrs.push(AttrInit { name, value });
            if self.eat(&TokenKind::Comma) {
                if !self.at_attr_start() {
                    return Ok(attrs);
                }
            } else if self.at_attr_start() {
                return Err(self.error_here(format!("expected `,` between attributes, found {}", self.peek_kind())));
            } else {
                return Ok(attrs);
            }
        }
    }

    /// `name:` followed by something other than the end of the line.
    fn at_attr_start(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::Ident(_)) && matches!(self.peek_nth(1), TokenKind::Colon)
    }

    fn peek_attr_name(&self) -> &'t str {
        match self.peek_kind() {
            TokenKind::Ident(name) => name,
            _ => "name",
        }
    }

    /// An optional trailing `:` followed by an indented block.
    fn at_block_start(&self) -> bool {
        self.check(&TokenKind::Colon) && matches!(self.peek_nth(1), TokenKind::Newline) && matches!(self.peek_nth(2), TokenKind::Indent)
    }

    // ------------------------------------------------------------- expressions

    fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_and()?;
        while self.eat(&TokenKind::Or) {
            let rhs = self.parse_and()?;
            lhs = logical(LogicalOp::Or, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_not()?;
        while self.eat(&TokenKind::And) {
            let rhs = self.parse_not()?;
            lhs = logical(LogicalOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_not(&mut self) -> PResult<Expr> {
        if self.check(&TokenKind::Not) {
            let start = self.bump().span;
            let expr = self.parse_not()?;
            let span = start.to(expr.span);
            return Ok(Expr { kind: ExprKind::Unary { op: UnaryOp::Not, expr: Box::new(expr) }, span });
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> PResult<Expr> {
        let lhs = self.parse_range()?;
        let Some(op) = comparison_op(self.peek_kind()) else { return Ok(lhs) };
        self.bump();
        let rhs = self.parse_range()?;
        if comparison_op(self.peek_kind()).is_some() {
            let d = Diagnostic::error("comparison operators cannot be chained", self.peek().span)
                .with_note("use `and` to combine comparisons: `a < b and b < c`");
            return Err(self.report(d));
        }
        Ok(binary(op, lhs, rhs))
    }

    fn parse_range(&mut self) -> PResult<Expr> {
        let lhs = self.parse_additive()?;
        if self.eat(&TokenKind::DotDot) {
            let rhs = self.parse_additive()?;
            return Ok(binary(BinaryOp::Range, lhs, rhs));
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Plus => BinaryOp::Add,
                TokenKind::Minus => BinaryOp::Sub,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.parse_multiplicative()?;
            lhs = binary(op, lhs, rhs);
        }
    }

    fn parse_multiplicative(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Star => BinaryOp::Mul,
                TokenKind::Slash => BinaryOp::Div,
                TokenKind::Percent => BinaryOp::Rem,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.parse_unary()?;
            lhs = binary(op, lhs, rhs);
        }
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        if self.check(&TokenKind::Minus) {
            let start = self.bump().span;
            let expr = self.parse_unary()?;
            let span = start.to(expr.span);
            // Fold negative literals right away so `-9223372036854775808` style
            // extremes and constant tables stay cheap.
            let kind = match expr.kind {
                ExprKind::Int(v) => ExprKind::Int(v.wrapping_neg()),
                ExprKind::Float(v) => ExprKind::Float(-v),
                _ => ExprKind::Unary { op: UnaryOp::Neg, expr: Box::new(expr) },
            };
            return Ok(Expr { kind, span });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek_kind() {
                TokenKind::LParen => {
                    self.bump();
                    let (args, named) = self.parse_call_args()?;
                    let span = expr.span.to(self.prev_span());
                    expr = Expr { kind: ExprKind::Call { callee: Box::new(expr), args, named }, span };
                }
                TokenKind::Question => {
                    self.bump();
                    let span = expr.span.to(self.prev_span());
                    expr = Expr { kind: ExprKind::Try(Box::new(expr)), span };
                }
                TokenKind::LBracket => {
                    self.bump();
                    let index = self.parse_expr()?;
                    self.expect(&TokenKind::RBracket, "`]` to close the index")?;
                    let span = expr.span.to(self.prev_span());
                    expr = Expr { kind: ExprKind::Index { target: Box::new(expr), index: Box::new(index) }, span };
                }
                TokenKind::Dot => {
                    self.bump();
                    let name = self.expect_ident("a field or method name after `.`")?;
                    if self.eat(&TokenKind::LParen) {
                        // A method of the value's type, else `f(x, a)` (uniform call syntax).
                        let (args, named) = self.parse_call_args()?;
                        let span = expr.span.to(self.prev_span());
                        expr = Expr { kind: ExprKind::MethodCall { receiver: Box::new(expr), method: name, args, named }, span };
                    } else {
                        let span = expr.span.to(name.span);
                        expr = Expr { kind: ExprKind::Field { target: Box::new(expr), name }, span };
                    }
                }
                _ => return Ok(expr),
            }
        }
    }

    /// Call arguments: positional ones, then `name: value` ones.
    fn parse_call_args(&mut self) -> PResult<CallArgs> {
        let mut args = Vec::new();
        let mut named: Vec<(Ident, Expr)> = Vec::new();
        while !self.check(&TokenKind::RParen) {
            if self.at_attr_start() {
                let name = self.expect_ident("an argument name")?;
                self.bump(); // `:`
                if named.iter().any(|(n, _)| n.name == name.name) {
                    self.report(Diagnostic::error(format!("`{}` is given more than once", name.name), name.span));
                }
                named.push((name, self.parse_expr()?));
            } else {
                let arg = self.parse_expr()?;
                if !named.is_empty() {
                    self.report(Diagnostic::error("positional arguments must come before named ones", arg.span));
                }
                args.push(arg);
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`)` to close the argument list")?;
        Ok((args, named))
    }

    fn parse_args(&mut self, close: &TokenKind, what: &str) -> PResult<Vec<Expr>> {
        let mut args = Vec::new();
        while !self.check(close) {
            args.push(self.parse_expr()?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(close, what)?;
        Ok(args)
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let tok = self.peek();
        let kind = match &tok.kind {
            TokenKind::Int(v) => ExprKind::Int(*v),
            TokenKind::Float(v) => ExprKind::Float(*v),
            TokenKind::True => ExprKind::Bool(true),
            TokenKind::False => ExprKind::Bool(false),
            TokenKind::Nil => ExprKind::Nil,
            TokenKind::Ident(name) => ExprKind::Ident(name.clone()),
            TokenKind::Str(segments) => {
                self.bump();
                return Ok(Expr { kind: self.parse_string(segments), span: tok.span });
            }
            TokenKind::LParen => {
                self.bump();
                let inner = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "`)` to close the parenthesis")?;
                return Ok(Expr { kind: inner.kind, span: tok.span.to(self.prev_span()) });
            }
            TokenKind::LBracket => {
                self.bump();
                let items = self.parse_args(&TokenKind::RBracket, "`]` to close the list")?;
                return Ok(Expr { kind: ExprKind::List(items), span: tok.span.to(self.prev_span()) });
            }
            TokenKind::LBrace => {
                self.bump();
                let mut entries = Vec::new();
                while !self.check(&TokenKind::RBrace) {
                    let key = self.parse_expr()?;
                    self.expect(&TokenKind::Colon, "`:` between a map key and its value")?;
                    entries.push((key, self.parse_expr()?));
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RBrace, "`}` to close the map")?;
                return Ok(Expr { kind: ExprKind::Map(entries), span: tok.span.to(self.prev_span()) });
            }
            TokenKind::Pipe => {
                self.bump();
                let mut params: Vec<Ident> = Vec::new();
                while !self.check(&TokenKind::Pipe) {
                    let p = self.expect_ident("a parameter name")?;
                    if params.iter().any(|q| q.name == p.name) {
                        self.report(Diagnostic::error(format!("duplicate parameter `{}`", p.name), p.span));
                    }
                    params.push(p);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::Pipe, "`|` to close the lambda's parameters")?;
                let body = self.parse_expr()?;
                if matches!(
                    self.peek_kind(),
                    TokenKind::Assign | TokenKind::PlusAssign | TokenKind::MinusAssign | TokenKind::StarAssign | TokenKind::SlashAssign
                ) {
                    let d = Diagnostic::error("the body of a lambda must be an expression", self.peek().span)
                        .with_label("assignment")
                        .with_note("for statements, declare a function: `fn reset()` with an indented body");
                    return Err(self.report(d));
                }
                let span = tok.span.to(body.span);
                return Ok(Expr { kind: ExprKind::Lambda { params, body: Box::new(body) }, span });
            }
            other => {
                let d = Diagnostic::error(format!("expected expression, found {other}"), tok.span).with_label("expected expression");
                return Err(self.report(d));
            }
        };
        self.bump();
        Ok(Expr { kind, span: tok.span })
    }

    fn parse_string(&mut self, segments: &[StrSegment]) -> ExprKind {
        if let [StrSegment::Text(text)] = segments {
            return ExprKind::Str(text.clone());
        }
        let mut parts = Vec::with_capacity(segments.len());
        for seg in segments {
            match seg {
                StrSegment::Text(text) => parts.push(InterpPart::Text(text.clone())),
                StrSegment::Expr { tokens, span } => {
                    let mut sub = Parser::new(tokens);
                    let parsed = sub.parse_expr();
                    if parsed.is_ok() && !sub.check(&TokenKind::Eof) {
                        let d = Diagnostic::error(format!("unexpected {} in interpolation", sub.peek_kind()), sub.peek().span);
                        sub.report(d);
                    }
                    self.diagnostics.append(&mut sub.diagnostics);
                    let expr = parsed.unwrap_or(Expr { kind: ExprKind::Nil, span: *span });
                    parts.push(InterpPart::Expr(expr));
                }
            }
        }
        ExprKind::Interp(parts)
    }
}

/// Whether an element name at the start of a statement begins an element
/// rather than an expression: element names are contextual keywords, so
/// `label = 3` or `image.width()` still treat them as variables.
pub(crate) fn starts_element(next: &TokenKind, after: &TokenKind) -> bool {
    use TokenKind as T;
    match next {
        T::Newline | T::Dedent | T::Eof | T::Arrow | T::Not | T::Minus => true,
        T::Str(_) | T::Int(_) | T::Float(_) | T::True | T::False | T::Nil | T::Ident(_) => true,
        T::Colon => matches!(after, T::Newline),
        _ => false,
    }
}

fn element_example(kind: UiKind) -> &'static str {
    match kind {
        UiKind::Label => "label \"Hello\"",
        UiKind::Button => "button \"Save\" -> save()",
        UiKind::Image => "image \"logo.png\"",
        UiKind::Input => "input name placeholder: \"Your name\"",
        UiKind::Checkbox => "checkbox done text: \"Done\"",
        UiKind::Switch => "switch dark text: \"Dark theme\"",
        UiKind::Slider => "slider volume min: 0, max: 100",
        UiKind::Progress => "progress 0.5",
        _ => "",
    }
}

fn comparison_op(kind: &TokenKind) -> Option<BinaryOp> {
    Some(match kind {
        TokenKind::EqEq => BinaryOp::Eq,
        TokenKind::NotEq => BinaryOp::Ne,
        TokenKind::Lt => BinaryOp::Lt,
        TokenKind::Le => BinaryOp::Le,
        TokenKind::Gt => BinaryOp::Gt,
        TokenKind::Ge => BinaryOp::Ge,
        _ => return None,
    })
}

fn binary(op: BinaryOp, lhs: Expr, rhs: Expr) -> Expr {
    let span = lhs.span.to(rhs.span);
    Expr { kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }, span }
}

fn logical(op: LogicalOp, lhs: Expr, rhs: Expr) -> Expr {
    let span = lhs.span.to(rhs.span);
    Expr { kind: ExprKind::Logical { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }, span }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(src: &str) -> Module {
        let out = parse(src);
        let errors: Vec<_> = out.diagnostics.iter().filter(|d| d.is_error()).collect();
        assert!(errors.is_empty(), "unexpected errors: {errors:#?}");
        out.module
    }

    fn expr(src: &str) -> ExprKind {
        let m = parse_ok(src);
        let StmtKind::Expr(e) = &m.stmts[0].kind else { panic!("not an expression: {:?}", m.stmts[0]) };
        e.kind.clone()
    }

    #[test]
    fn precedence() {
        let ExprKind::Binary { op: BinaryOp::Add, rhs, .. } = expr("1 + 2 * 3") else { panic!() };
        assert!(matches!(rhs.kind, ExprKind::Binary { op: BinaryOp::Mul, .. }));
        let ExprKind::Logical { op: LogicalOp::Or, lhs, .. } = expr("a and b or not c") else { panic!() };
        assert!(matches!(lhs.kind, ExprKind::Logical { op: LogicalOp::And, .. }));
        assert!(matches!(expr("0..n + 1"), ExprKind::Binary { op: BinaryOp::Range, .. }));
    }

    #[test]
    fn methods_and_fields() {
        let ExprKind::MethodCall { receiver, method, args, .. } = expr("items.push(4)") else { panic!() };
        assert_eq!((receiver.kind, method.name.as_str(), args.len()), (ExprKind::Ident("items".into()), "push", 1));
        let ExprKind::Field { name, .. } = expr("p.x") else { panic!() };
        assert_eq!(name.name, "x");
        assert!(matches!(expr("read(path)?"), ExprKind::Try(_)));
        let ExprKind::Call { args, named, .. } = expr("Point(1, y: 2)") else { panic!() };
        assert_eq!((args.len(), named[0].0.name.as_str()), (1, "y"));
    }

    #[test]
    fn maps_and_lambdas() {
        let ExprKind::Map(entries) = expr("{\"a\": 1, \"b\": {}}") else { panic!() };
        assert_eq!(entries.len(), 2);
        let ExprKind::Lambda { params, body } = expr("|a, b| a + b") else { panic!() };
        assert_eq!(params.len(), 2);
        assert!(matches!(body.kind, ExprKind::Binary { op: BinaryOp::Add, .. }));
        assert!(matches!(expr("|| 1"), ExprKind::Lambda { .. }));
        let ExprKind::Interp(parts) = expr("\"{ {1: 2}[1] }\"") else { panic!() };
        assert!(matches!(&parts[0], InterpPart::Expr(Expr { kind: ExprKind::Index { .. }, .. })));
    }

    #[test]
    fn types_and_match() {
        let src = "\
struct Point
    x
    y = 0

    fn len(self)
        return self.x

enum Shape
    Circle(r)
    Rect(w, h)
    Empty

match s
    Shape.Circle(r) if r > 1 => print(r)
    Rect(w, _) | Empty
        print(w)
    0..10 => print(\"small\")
    -1 | \"x\" | nil => return
    _ => print(\"other\")
if let Some(x) = m.get(1)
    print(x)
";
        let m = parse_ok(src);
        let StmtKind::Struct(p) = &m.stmts[0].kind else { panic!() };
        assert_eq!((p.kind, p.fields.len(), p.methods.len()), (TypeKind::Struct, 2, 1));
        assert!(p.fields[1].default.is_some());
        let StmtKind::Struct(e) = &m.stmts[1].kind else { panic!() };
        assert_eq!(e.variants.iter().map(|v| v.fields.len()).collect::<Vec<_>>(), [1, 2, 0]);
        let StmtKind::Match { arms, .. } = &m.stmts[2].kind else { panic!() };
        assert_eq!(arms.len(), 5);
        assert!(arms[0].guard.is_some());
        assert!(matches!(&arms[1].pattern.kind, PatternKind::Or(alts) if alts.len() == 2));
        assert!(matches!(arms[2].pattern.kind, PatternKind::Range(..)));
        assert!(matches!(arms[4].pattern.kind, PatternKind::Wildcard));
        let StmtKind::If { branches, .. } = &m.stmts[3].kind else { panic!() };
        assert!(matches!(branches[0].0, Cond::Let(..)));
    }

    #[test]
    fn nested_functions_and_imports() {
        let m = parse_ok("import utils\nimport math.{clamp, lerp}\nfn outer()\n    fn inner(x)\n        return x\n    return inner(1)\n");
        assert!(matches!(&m.stmts[1].kind, StmtKind::Import(Import { names, .. }) if names.len() == 2));
        let StmtKind::Fn(f) = &m.stmts[2].kind else { panic!() };
        assert!(matches!(f.body.stmts[0].kind, StmtKind::Fn(_)));
        assert!(parse("if true\n    struct A\n        x\n").diagnostics.iter().any(|d| d.message.contains("top level")));
    }

    #[test]
    fn functions_and_control_flow() {
        let src = "\
fn fib(n)
    if n < 2
        return n
    elif n == 2:
        return 1
    else
        return fib(n - 1) + fib(n - 2)

for i in 0..10
    if i % 2 == 0
        continue
    print(fib(i))
";
        let m = parse_ok(src);
        assert_eq!(m.stmts.len(), 2);
        let StmtKind::Fn(f) = &m.stmts[0].kind else { panic!() };
        assert_eq!(f.params.len(), 1);
        let StmtKind::If { branches, otherwise } = &f.body.stmts[0].kind else { panic!() };
        assert_eq!(branches.len(), 2);
        assert!(otherwise.is_some());
    }

    #[test]
    fn interpolation() {
        let ExprKind::Interp(parts) = expr(r#""n = {n * 2}!""#) else { panic!() };
        assert_eq!(parts.len(), 3);
        assert!(matches!(&parts[1], InterpPart::Expr(Expr { kind: ExprKind::Binary { .. }, .. })));
    }

    #[test]
    fn ui_tree() {
        let src = "\
view
    window \"Demo\"
        column
            label \"Count: {count}\"
            row
                button \"+\" -> add(1)
                button \"-\" -> add(-1)
";
        let m = parse_ok(src);
        let StmtKind::View(body) = &m.stmts[0].kind else { panic!() };
        let StmtKind::Ui(win) = &body.stmts[0].kind else { panic!() };
        assert_eq!(win.kind, UiKind::Window);
        let col = &win.children.as_ref().unwrap().stmts[0];
        let StmtKind::Ui(col) = &col.kind else { panic!() };
        assert_eq!(col.children.as_ref().unwrap().stmts.len(), 2);
    }

    #[test]
    fn attributes_inline_and_in_blocks() {
        let src = "\
view
    card padding: 16, style: \"outlined\"
        radius: 12
        label \"Hi\" size: 20, color: \"primary\",
        button \"Go\" icon: \"send\" -> go(1)
            tooltip: \"Send it\"
        spacer
        input query placeholder: \"Search\"
";
        let m = parse_ok(src);
        let StmtKind::View(body) = &m.stmts[0].kind else { panic!() };
        let StmtKind::Ui(card) = &body.stmts[0].kind else { panic!() };
        assert_eq!(card.kind, UiKind::Card);
        assert!(card.arg.is_none());
        assert_eq!(card.attrs.iter().map(|a| a.name.name.as_str()).collect::<Vec<_>>(), ["padding", "style"]);
        let kids = &card.children.as_ref().unwrap().stmts;
        assert!(matches!(&kids[0].kind, StmtKind::Attrs(a) if a[0].name.name == "radius"));
        let StmtKind::Ui(label) = &kids[1].kind else { panic!() };
        assert_eq!((label.kind, label.attrs.len()), (UiKind::Label, 2));
        let StmtKind::Ui(button) = &kids[2].kind else { panic!() };
        assert!(button.action.is_some() && button.children.is_some());
        assert!(matches!(&kids[3].kind, StmtKind::Ui(UiElement { kind: UiKind::Spacer, arg: None, .. })));
        let StmtKind::Ui(input) = &kids[4].kind else { panic!() };
        assert!(matches!(&input.arg, Some(Expr { kind: ExprKind::Ident(n), .. }) if n == "query"));
    }

    #[test]
    fn element_names_are_contextual() {
        let m = parse_ok("let label = \"x\"\nlabel = label + \"!\"\nimage.size()\nlabel label\ncolumn:\n    row\n");
        assert!(matches!(m.stmts[0].kind, StmtKind::Let { .. }));
        assert!(matches!(m.stmts[1].kind, StmtKind::Assign { .. }));
        assert!(matches!(m.stmts[2].kind, StmtKind::Expr(_)));
        assert!(matches!(&m.stmts[3].kind, StmtKind::Ui(UiElement { kind: UiKind::Label, arg: Some(_), .. })));
        assert!(matches!(&m.stmts[4].kind, StmtKind::Ui(UiElement { kind: UiKind::Column, children: Some(_), .. })));
    }

    #[test]
    fn rejects_bad_elements() {
        let has = |src: &str, needle: &str| {
            let out = parse(src);
            assert!(out.diagnostics.iter().any(|d| d.message.contains(needle)), "{src:?}: {:#?}", out.diagnostics);
        };
        has("button \"x\" -> f() size: 3\n", "before the event handler");
        has("label \"x\" size: 3 color: \"red\"\n", "expected `,` between attributes");
        has("row \"title\"\n", "`row` does not take an argument");
        has("label \"x\" -> f()\n", "does not accept an event handler");
        has("image\n", "`image` requires an argument (a string)");
    }

    #[test]
    fn assignments() {
        let m = parse_ok("x = 1\nx += 2\nxs[0] *= 3\n");
        assert!(matches!(m.stmts[1].kind, StmtKind::Assign { op: Some(BinaryOp::Add), .. }));
        assert!(matches!(&m.stmts[2].kind, StmtKind::Assign { target: Expr { kind: ExprKind::Index { .. }, .. }, .. }));
    }

    #[test]
    fn recovers_after_errors() {
        let out = parse("let = 3\nlet ok = 1\nif x +\n    x\nprint(ok)\nf(1 2)\n");
        assert!(out.diagnostics.len() >= 3, "{:#?}", out.diagnostics);
        // `let ok` and `print(ok)` survived.
        assert!(out.module.stmts.len() >= 2);
    }

    #[test]
    fn rejects_bad_constructs() {
        assert!(parse("1 < 2 < 3\n").diagnostics.iter().any(|d| d.message.contains("chained")));
        assert!(parse("break\n").diagnostics.iter().any(|d| d.message.contains("outside of a loop")));
        assert!(parse("f() = 3\n").diagnostics.iter().any(|d| d.message.contains("assignment target")));
        assert!(parse("label\n").diagnostics.iter().any(|d| d.message.contains("`label` requires an argument")));
    }
}
