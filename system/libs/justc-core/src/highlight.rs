//! Error-tolerant syntax classification for editors.

use crate::bytecode::builtin_index;
use crate::lexer;
use crate::parser::starts_element;
use crate::prelude::*;
use crate::span::Span;
use crate::token::{StrSegment, Token, TokenKind};
use crate::ui::UiKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HighlightKind {
    Keyword,
    /// `window`, `button`, ... and `view`.
    UiPrimitive,
    /// `padding` in `padding: 16`.
    Attribute,
    Identifier,
    /// Identifier in call position.
    Function,
    Builtin,
    Number,
    String,
    Constant,
    Operator,
    Punctuation,
    Comment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub span: Span,
    pub kind: HighlightKind,
}

/// Classify every token of `src`. Spans are sorted and non-overlapping;
/// whitespace and unrecognised characters are left uncovered.
pub fn highlight(src: &str) -> Vec<HighlightSpan> {
    let lexed = lexer::tokenize(src);
    let mut out = Vec::with_capacity(lexed.tokens.len() + lexed.comments.len());
    classify(&lexed.tokens, &mut out);
    out.extend(lexed.comments.iter().map(|&span| HighlightSpan { span, kind: HighlightKind::Comment }));
    out.sort_by_key(|h| h.span.start);
    out
}

fn classify(tokens: &[Token], out: &mut Vec<HighlightSpan>) {
    use TokenKind as T;
    let kind_at = |i: usize| tokens.get(i).map_or(&T::Eof, |t| &t.kind);
    for (i, tok) in tokens.iter().enumerate() {
        let line_start = i == 0 || matches!(kind_at(i - 1), T::Newline | T::Indent | T::Dedent);
        let kind = match &tok.kind {
            T::Newline | T::Indent | T::Dedent | T::Eof => continue,
            T::Str(segments) => {
                push_string(tok.span, segments, out);
                continue;
            }
            T::Int(_) | T::Float(_) => HighlightKind::Number,
            T::True | T::False | T::Nil => HighlightKind::Constant,
            T::Ident(_) if matches!(kind_at(i + 1), T::Colon) && !matches!(kind_at(i + 2), T::Newline | T::Eof) => HighlightKind::Attribute,
            T::Ident(name) if line_start && UiKind::from_name(name).is_some() && starts_element(kind_at(i + 1), kind_at(i + 2)) => {
                HighlightKind::UiPrimitive
            }
            T::Ident(name) => {
                let is_call = matches!(tokens.get(i + 1), Some(Token { kind: T::LParen, .. }));
                match (is_call, builtin_index(name).is_some()) {
                    (true, true) => HighlightKind::Builtin,
                    (true, false) => HighlightKind::Function,
                    _ => HighlightKind::Identifier,
                }
            }
            T::View => HighlightKind::UiPrimitive,
            k if k.is_keyword() => HighlightKind::Keyword,
            T::LParen | T::RParen | T::LBracket | T::RBracket | T::Comma | T::Colon | T::Dot => HighlightKind::Punctuation,
            _ => HighlightKind::Operator,
        };
        out.push(HighlightSpan { span: tok.span, kind });
    }
}

fn push_string(span: Span, segments: &[StrSegment], out: &mut Vec<HighlightSpan>) {
    let mut cursor = span.start;
    for seg in segments {
        if let StrSegment::Expr { tokens, span: inner } = seg {
            if inner.start > cursor {
                out.push(HighlightSpan { span: Span { start: cursor, end: inner.start }, kind: HighlightKind::String });
            }
            classify(tokens, out);
            cursor = inner.end;
        }
    }
    if span.end > cursor {
        out.push(HighlightSpan { span: Span { start: cursor, end: span.end }, kind: HighlightKind::String });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_and_orders() {
        let src = "fn f(x) # c\n    print(\"v={x + 1}\")\n";
        let hl = highlight(src);
        let text = |h: &HighlightSpan| &src[h.span.range()];
        assert!(hl.windows(2).all(|w| w[0].span.end <= w[1].span.start));
        assert!(hl.iter().any(|h| h.kind == HighlightKind::Comment && text(h) == "# c"));
        assert!(hl.iter().any(|h| h.kind == HighlightKind::Builtin && text(h) == "print"));
        assert!(hl.iter().any(|h| h.kind == HighlightKind::Identifier && text(h) == "x" && h.span.start > 20));
        assert!(hl.iter().any(|h| h.kind == HighlightKind::String && text(h) == "\"v={"));
    }

    #[test]
    fn elements_and_attributes_are_contextual() {
        let src = "view\n    label label size: 3\n    label = 1\n";
        let hl = highlight(src);
        let kinds: Vec<_> = hl.iter().map(|h| (&src[h.span.range()], h.kind)).collect();
        assert!(kinds.contains(&("view", HighlightKind::UiPrimitive)));
        assert_eq!(kinds[1], ("label", HighlightKind::UiPrimitive));
        assert_eq!(kinds[2], ("label", HighlightKind::Identifier));
        assert_eq!(kinds[3], ("size", HighlightKind::Attribute));
        assert!(kinds.iter().filter(|k| k.1 == HighlightKind::UiPrimitive).count() == 2, "{kinds:?}");
    }
}
