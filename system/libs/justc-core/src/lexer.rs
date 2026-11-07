//! Hand-written lexer with Python-style significant indentation.
//!
//! Layout rules:
//! * A logical line ends with [`TokenKind::Newline`].
//! * An increase of indentation opens a block ([`TokenKind::Indent`]), a
//!   decrease closes one or more blocks ([`TokenKind::Dedent`]).
//! * Blank and comment-only lines are ignored, and newlines inside `(...)` /
//!   `[...]` are implicit line continuations.
//! * Tabs count as four columns.
//!
//! String literals support `{expr}` interpolation; the embedded expression is
//! lexed recursively into its own token stream (see [`StrSegment`]).
//!
//! The lexer never aborts: errors are collected as diagnostics and lexing
//! continues, which is what an IDE needs for highlighting broken code.

use crate::diagnostic::Diagnostic;
use crate::prelude::*;
use crate::span::Span;
use crate::token::{StrSegment, Token, TokenKind};

const TAB_WIDTH: u32 = 4;

#[derive(Debug, Default)]
pub struct LexOutput {
    pub tokens: Vec<Token>,
    /// Spans of `#` comments (used for highlighting).
    pub comments: Vec<Span>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Tokenize a complete source file.
pub fn tokenize(src: &str) -> LexOutput {
    Lexer::new(src).run()
}

/// Tokenize a file whose spans start at `base` (an imported file, see
/// [`SourceFile::add`](crate::span::SourceFile::add)).
pub fn tokenize_at(src: &str, base: u32) -> LexOutput {
    let mut out = tokenize(src);
    if base > 0 {
        let shift = |s: &mut Span| {
            s.start += base;
            s.end += base;
        };
        fn shift_tokens(tokens: &mut [Token], shift: &impl Fn(&mut Span)) {
            for t in tokens {
                shift(&mut t.span);
                if let TokenKind::Str(segments) = &mut t.kind {
                    for seg in segments {
                        if let StrSegment::Expr { tokens, span } = seg {
                            shift(span);
                            shift_tokens(tokens, shift);
                        }
                    }
                }
            }
        }
        shift_tokens(&mut out.tokens, &shift);
        out.comments.iter_mut().for_each(shift);
        out.diagnostics.iter_mut().for_each(|d| shift(&mut d.span));
    }
    out
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
    comments: Vec<Span>,
    diagnostics: Vec<Diagnostic>,
    indent_stack: Vec<u32>,
    nesting: u32,
    at_line_start: bool,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Lexer {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            tokens: Vec::with_capacity(src.len() / 3),
            comments: Vec::new(),
            diagnostics: Vec::new(),
            indent_stack: vec![0],
            nesting: 0,
            at_line_start: true,
        }
    }

    fn run(mut self) -> LexOutput {
        // Skip a UTF-8 byte order mark.
        if self.src.starts_with('\u{feff}') {
            self.pos = 3;
        }
        loop {
            if self.at_line_start && self.nesting == 0 {
                self.at_line_start = false;
                self.handle_indentation();
            }
            match self.peek() {
                None => break,
                Some(b' ' | b'\t' | b'\r') => self.pos += 1,
                Some(b'\n') => {
                    let start = self.pos;
                    self.pos += 1;
                    if self.nesting == 0 {
                        self.push_newline(start);
                        self.at_line_start = true;
                    }
                }
                Some(b'#') => self.skip_comment(),
                Some(_) => {
                    if let Some(tok) = self.scan_token() {
                        self.tokens.push(tok);
                    }
                }
            }
        }
        let end = self.src.len();
        self.push_newline(end);
        while self.indent_stack.len() > 1 {
            self.indent_stack.pop();
            self.push(TokenKind::Dedent, end, end);
        }
        self.push(TokenKind::Eof, end, end);
        LexOutput { tokens: self.tokens, comments: self.comments, diagnostics: self.diagnostics }
    }

    // ---------------------------------------------------------------- layout

    fn handle_indentation(&mut self) {
        let mut col = 0u32;
        while let Some(b) = self.peek() {
            match b {
                b' ' => col += 1,
                b'\t' => col = (col / TAB_WIDTH + 1) * TAB_WIDTH,
                _ => break,
            }
            self.pos += 1;
        }
        // Blank or comment-only lines do not affect indentation.
        if matches!(self.peek(), None | Some(b'\n' | b'\r' | b'#')) {
            return;
        }
        let top = *self.indent_stack.last().expect("indent stack is never empty");
        if col > top {
            self.indent_stack.push(col);
            self.push(TokenKind::Indent, self.pos, self.pos);
        } else if col < top {
            while *self.indent_stack.last().unwrap() > col {
                self.indent_stack.pop();
                self.push(TokenKind::Dedent, self.pos, self.pos);
            }
            if *self.indent_stack.last().unwrap() != col {
                self.diagnostics.push(
                    Diagnostic::error("unindent does not match any outer indentation level", Span::new(self.pos, self.pos + 1))
                        .with_label("inconsistent indentation"),
                );
                self.indent_stack.push(col);
            }
        }
    }

    fn push_newline(&mut self, at: usize) {
        let needed = matches!(self.tokens.last(), Some(t) if !matches!(t.kind, TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent));
        if needed {
            self.push(TokenKind::Newline, at, at);
        }
    }

    fn skip_comment(&mut self) {
        let start = self.pos;
        while !matches!(self.peek(), None | Some(b'\n')) {
            self.pos += 1;
        }
        self.comments.push(Span::new(start, self.pos));
    }

    // ---------------------------------------------------------------- tokens

    fn scan_token(&mut self) -> Option<Token> {
        let start = self.pos;
        let c = self.peek_char()?;
        if c.is_ascii_digit() {
            return Some(self.scan_number());
        }
        if c == '"' {
            return Some(self.scan_string());
        }
        if is_ident_start(c) {
            return Some(self.scan_ident());
        }
        self.pos += c.len_utf8();
        let next = self.peek();
        let two = |lexer: &mut Self, kind| {
            lexer.pos += 1;
            kind
        };
        use TokenKind as T;
        let kind = match c {
            '(' => {
                self.nesting += 1;
                T::LParen
            }
            ')' => {
                self.nesting = self.nesting.saturating_sub(1);
                T::RParen
            }
            '[' => {
                self.nesting += 1;
                T::LBracket
            }
            ']' => {
                self.nesting = self.nesting.saturating_sub(1);
                T::RBracket
            }
            '{' => {
                self.nesting += 1;
                T::LBrace
            }
            '}' => {
                self.nesting = self.nesting.saturating_sub(1);
                T::RBrace
            }
            '|' => T::Pipe,
            '?' => T::Question,
            ',' => T::Comma,
            ':' => T::Colon,
            '%' => T::Percent,
            '.' if next == Some(b'.') => two(self, T::DotDot),
            '.' => T::Dot,
            '+' if next == Some(b'=') => two(self, T::PlusAssign),
            '+' => T::Plus,
            '-' if next == Some(b'=') => two(self, T::MinusAssign),
            '-' if next == Some(b'>') => two(self, T::Arrow),
            '-' => T::Minus,
            '*' if next == Some(b'=') => two(self, T::StarAssign),
            '*' => T::Star,
            '/' if next == Some(b'=') => two(self, T::SlashAssign),
            '/' => T::Slash,
            '=' if next == Some(b'=') => two(self, T::EqEq),
            '=' if next == Some(b'>') => two(self, T::FatArrow),
            '=' => T::Assign,
            '!' if next == Some(b'=') => two(self, T::NotEq),
            '<' if next == Some(b'=') => two(self, T::Le),
            '<' => T::Lt,
            '>' if next == Some(b'=') => two(self, T::Ge),
            '>' => T::Gt,
            _ => {
                let mut d = Diagnostic::error(format!("unexpected character `{}`", c.escape_debug()), Span::new(start, self.pos));
                if c == '!' {
                    d = d.with_note("use `not` for boolean negation");
                } else if c == '&' {
                    d = d.with_note("use `and` / `or` for boolean logic");
                }
                self.diagnostics.push(d);
                return None;
            }
        };
        Some(Token { kind, span: Span::new(start, self.pos) })
    }

    fn scan_ident(&mut self) -> Token {
        let start = self.pos;
        while let Some(c) = self.peek_char() {
            if !is_ident_continue(c) {
                break;
            }
            self.pos += c.len_utf8();
        }
        let text = &self.src[start..self.pos];
        let kind = TokenKind::keyword(text).unwrap_or_else(|| TokenKind::Ident(text.to_owned()));
        Token { kind, span: Span::new(start, self.pos) }
    }

    fn scan_number(&mut self) -> Token {
        let start = self.pos;
        let radix = match (self.peek(), self.peek_at(1)) {
            (Some(b'0'), Some(b'x' | b'X')) => 16,
            (Some(b'0'), Some(b'b' | b'B')) => 2,
            (Some(b'0'), Some(b'o' | b'O')) => 8,
            _ => 10,
        };
        let kind = if radix != 10 {
            self.pos += 2;
            let digits_start = self.pos;
            while matches!(self.peek(), Some(b) if (b as char).is_digit(radix) || b == b'_') {
                self.pos += 1;
            }
            let digits: String = self.src[digits_start..self.pos].chars().filter(|&c| c != '_').collect();
            match i64::from_str_radix(&digits, radix) {
                Ok(v) => TokenKind::Int(v),
                Err(_) => {
                    let msg = if digits.is_empty() { "missing digits after integer base prefix" } else { "integer literal is too large" };
                    self.diagnostics.push(Diagnostic::error(msg, Span::new(start, self.pos)));
                    TokenKind::Int(0)
                }
            }
        } else {
            self.eat_digits();
            let mut is_float = false;
            // `1..5` is a range, not a float: require a digit after the dot.
            if self.peek() == Some(b'.') && matches!(self.peek_at(1), Some(b'0'..=b'9')) {
                is_float = true;
                self.pos += 1;
                self.eat_digits();
            }
            if matches!(self.peek(), Some(b'e' | b'E')) {
                let save = self.pos;
                self.pos += 1;
                if matches!(self.peek(), Some(b'+' | b'-')) {
                    self.pos += 1;
                }
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    is_float = true;
                    self.eat_digits();
                } else {
                    self.pos = save;
                }
            }
            let text: String = self.src[start..self.pos].chars().filter(|&c| c != '_').collect();
            if is_float {
                TokenKind::Float(text.parse().unwrap_or(f64::NAN))
            } else {
                match text.parse::<i64>() {
                    Ok(v) => TokenKind::Int(v),
                    Err(_) => {
                        self.diagnostics.push(
                            Diagnostic::error("integer literal is too large", Span::new(start, self.pos))
                                .with_note("integers are 64-bit signed values"),
                        );
                        TokenKind::Int(0)
                    }
                }
            }
        };
        if let Some(c) = self.peek_char() {
            if is_ident_start(c) {
                let suffix_start = self.pos;
                while matches!(self.peek_char(), Some(c) if is_ident_continue(c)) {
                    self.pos += self.peek_char().unwrap().len_utf8();
                }
                self.diagnostics.push(Diagnostic::error("invalid suffix on numeric literal", Span::new(suffix_start, self.pos)));
            }
        }
        Token { kind, span: Span::new(start, self.pos) }
    }

    fn eat_digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9' | b'_')) {
            self.pos += 1;
        }
    }

    fn scan_string(&mut self) -> Token {
        let start = self.pos;
        self.pos += 1; // opening quote
        let mut segments = Vec::new();
        let mut buf = String::new();
        loop {
            match self.peek_char() {
                None | Some('\n') => {
                    self.diagnostics.push(
                        Diagnostic::error("unterminated string literal", Span::new(start, self.pos)).with_label("string starts here"),
                    );
                    break;
                }
                Some('"') => {
                    self.pos += 1;
                    break;
                }
                Some('\\') => {
                    if let Some(c) = self.scan_escape() {
                        buf.push(c);
                    }
                }
                Some('{') => {
                    if !buf.is_empty() {
                        segments.push(StrSegment::Text(core::mem::take(&mut buf)));
                    }
                    self.pos += 1;
                    segments.push(self.scan_interpolation());
                }
                Some('}') => {
                    self.diagnostics.push(
                        Diagnostic::error("unmatched `}` in string literal", Span::new(self.pos, self.pos + 1))
                            .with_note("write `\\}` for a literal brace"),
                    );
                    self.pos += 1;
                }
                Some(c) => {
                    buf.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
        if !buf.is_empty() || segments.is_empty() {
            segments.push(StrSegment::Text(buf));
        }
        Token { kind: TokenKind::Str(segments), span: Span::new(start, self.pos) }
    }

    fn scan_escape(&mut self) -> Option<char> {
        let start = self.pos;
        self.pos += 1; // backslash
        let c = self.peek_char();
        if let Some(c) = c {
            self.pos += c.len_utf8();
        }
        let out = match c {
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            Some('0') => '\0',
            Some('\\') => '\\',
            Some('"') => '"',
            Some('{') => '{',
            Some('}') => '}',
            Some('u') => return self.scan_unicode_escape(start),
            _ => {
                self.diagnostics.push(Diagnostic::error("unknown escape sequence", Span::new(start, self.pos)));
                return None;
            }
        };
        Some(out)
    }

    fn scan_unicode_escape(&mut self, start: usize) -> Option<char> {
        if self.peek() != Some(b'{') {
            self.diagnostics.push(Diagnostic::error("expected `{` after `\\u`", Span::new(start, self.pos)));
            return None;
        }
        self.pos += 1;
        let digits_start = self.pos;
        while matches!(self.peek(), Some(b) if b.is_ascii_hexdigit()) {
            self.pos += 1;
        }
        let digits = &self.src[digits_start..self.pos];
        if self.peek() == Some(b'}') {
            self.pos += 1;
        }
        let ch = u32::from_str_radix(digits, 16).ok().and_then(char::from_u32);
        if ch.is_none() {
            self.diagnostics.push(Diagnostic::error("invalid unicode escape", Span::new(start, self.pos)));
        }
        ch
    }

    /// Lex the tokens of `{ expr }`; the opening brace is already consumed.
    fn scan_interpolation(&mut self) -> StrSegment {
        let saved_nesting = core::mem::replace(&mut self.nesting, 0);
        let start = self.pos;
        let mut tokens = Vec::new();
        let mut braces = 0u32;
        let end;
        loop {
            match self.peek() {
                Some(b' ' | b'\t') => self.pos += 1,
                Some(b'}') if braces == 0 => {
                    end = self.pos;
                    self.pos += 1;
                    break;
                }
                None | Some(b'\n') => {
                    self.diagnostics.push(
                        Diagnostic::error("unterminated string interpolation", Span::new(start.saturating_sub(1), self.pos))
                            .with_note("close the interpolation with `}`"),
                    );
                    end = self.pos;
                    break;
                }
                _ => {
                    if let Some(tok) = self.scan_token() {
                        // Map literals nest braces inside the interpolation.
                        match tok.kind {
                            TokenKind::LBrace => braces += 1,
                            TokenKind::RBrace => braces = braces.saturating_sub(1),
                            _ => {}
                        }
                        tokens.push(tok);
                    }
                }
            }
        }
        if tokens.is_empty() {
            self.diagnostics.push(Diagnostic::error("empty interpolation `{}`", Span::new(start.saturating_sub(1), self.pos)));
        }
        tokens.push(Token { kind: TokenKind::Eof, span: Span::new(end, end) });
        self.nesting = saved_nesting;
        StrSegment::Expr { tokens, span: Span::new(start, end) }
    }

    // --------------------------------------------------------------- helpers

    #[inline]
    fn push(&mut self, kind: TokenKind, start: usize, end: usize) {
        self.tokens.push(Token { kind, span: Span::new(start, end) });
    }

    #[inline]
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    #[inline]
    fn peek_at(&self, n: usize) -> Option<u8> {
        self.bytes.get(self.pos + n).copied()
    }

    #[inline]
    fn peek_char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let out = tokenize(src);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        out.tokens.into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn indentation_produces_blocks() {
        let src = "if x\n    a\n    if y\n        b\nc\n";
        assert_eq!(
            kinds(src),
            vec![
                If,
                Ident("x".into()),
                Newline,
                Indent,
                Ident("a".into()),
                Newline,
                If,
                Ident("y".into()),
                Newline,
                Indent,
                Ident("b".into()),
                Newline,
                Dedent,
                Dedent,
                Ident("c".into()),
                Newline,
                Eof
            ]
        );
    }

    #[test]
    fn blank_and_comment_lines_are_ignored() {
        let src = "fn f()\n\n    # comment\n    return 1\n\n# trailing\n";
        let k = kinds(src);
        assert_eq!(k.iter().filter(|k| **k == Indent).count(), 1);
        assert_eq!(k.iter().filter(|k| **k == Dedent).count(), 1);
        assert_eq!(tokenize(src).comments.len(), 2);
    }

    #[test]
    fn newlines_inside_brackets_are_joined() {
        let k = kinds("let a = [1,\n    2,\n  3]\n");
        assert_eq!(k.iter().filter(|k| **k == Newline).count(), 1);
        assert!(!k.contains(&Indent));
    }

    #[test]
    fn numbers_and_ranges() {
        assert_eq!(kinds("0..10")[..3], [Int(0), DotDot, Int(10)]);
        assert_eq!(kinds("1.5e3")[0], Float(1500.0));
        assert_eq!(kinds("0xff_ff")[0], Int(0xffff));
        assert_eq!(kinds("1_000")[0], Int(1000));
        assert!(!tokenize("99999999999999999999").diagnostics.is_empty());
    }

    #[test]
    fn string_interpolation_is_lexed_recursively() {
        let toks = kinds(r#""Hi {name}, {f("x")}!""#);
        let Str(segs) = &toks[0] else { panic!() };
        assert_eq!(segs.len(), 5);
        assert_eq!(segs[0], StrSegment::Text("Hi ".into()));
        let StrSegment::Expr { tokens, .. } = &segs[3] else { panic!() };
        assert!(matches!(tokens[2].kind, Str(_)));
        assert_eq!(segs[4], StrSegment::Text("!".into()));
    }

    #[test]
    fn escapes() {
        let toks = kinds(r#""a\n\{b\}\u{1F600}""#);
        assert_eq!(toks[0], Str(vec![StrSegment::Text("a\n{b}😀".into())]));
    }

    #[test]
    fn errors_are_recovered() {
        let out = tokenize("let x = \"abc\nlet y = 2 $\n");
        assert_eq!(out.diagnostics.len(), 2);
        assert!(out.tokens.iter().any(|t| t.kind == Ident("y".into())));
    }

    #[test]
    fn inconsistent_dedent_is_reported() {
        let out = tokenize("if a\n    b\n  c\n");
        assert_eq!(out.diagnostics.len(), 1);
    }

    #[test]
    fn operators() {
        assert_eq!(
            kinds("a += 1 -> b != c <= d")[..10],
            [Ident("a".into()), PlusAssign, Int(1), Arrow, Ident("b".into()), NotEq, Ident("c".into()), Le, Ident("d".into()), Newline]
        );
    }
}
