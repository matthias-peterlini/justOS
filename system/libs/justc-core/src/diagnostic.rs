//! Compiler diagnostics with rustc-style rendering.

use crate::prelude::*;
use crate::span::{SourceFile, Span};
use core::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Span,
    /// Short text printed under the caret.
    pub label: Option<String>,
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>, span: Span) -> Self {
        Diagnostic { severity: Severity::Error, message: message.into(), span, label: None, notes: Vec::new() }
    }

    pub fn warning(message: impl Into<String>, span: Span) -> Self {
        Diagnostic { severity: Severity::Warning, ..Diagnostic::error(message, span) }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// Render the diagnostic with a source excerpt and caret underline.
    pub fn render(&self, file: &SourceFile) -> String {
        let (line, col) = file.line_col(self.span.start);
        let src_line = file.line_text_at(self.span.start);
        let gutter = line.to_string().len();
        let kind = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let mut out = String::new();
        let _ = writeln!(out, "{kind}: {}", self.message);
        let _ = writeln!(out, "{:gutter$}--> {}:{line}:{col}", "", file.name_at(self.span.start));
        let _ = writeln!(out, "{:gutter$} |", "");
        let _ = writeln!(out, "{line:>gutter$} | {src_line}");
        // Underline only the part of the span that lies on the first line.
        let line_chars = src_line.chars().count() as u32;
        let (end_line, end_col) = file.line_col(self.span.end);
        let width = if end_line == line { end_col.saturating_sub(col).max(1) } else { (line_chars + 1).saturating_sub(col).max(1) };
        let pad: String = src_line.chars().take(col as usize - 1).map(|c| if c == '\t' { '\t' } else { ' ' }).collect();
        let _ = write!(out, "{:gutter$} | {pad}{}", "", "^".repeat(width as usize));
        if let Some(label) = &self.label {
            let _ = write!(out, " {label}");
        }
        out.push('\n');
        for note in &self.notes {
            let _ = writeln!(out, "{:gutter$} = note: {note}", "");
        }
        out
    }
}

/// Render a whole batch of diagnostics, followed by a summary line.
pub fn render_all(diagnostics: &[Diagnostic], file: &SourceFile) -> String {
    let mut out = String::new();
    for d in diagnostics {
        out.push_str(&d.render(file));
        out.push('\n');
    }
    let errors = diagnostics.iter().filter(|d| d.is_error()).count();
    if errors > 0 {
        let _ = writeln!(
            out,
            "error: could not compile `{}` due to {errors} previous error{}",
            file.name(),
            if errors == 1 { "" } else { "s" }
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_caret() {
        let f = SourceFile::new("main.jt", "let x = 1\nlet y = ?\n");
        let d = Diagnostic::error("unexpected character `?`", Span::new(18, 19)).with_label("here");
        let r = d.render(&f);
        assert!(r.contains("--> main.jt:2:9"), "{r}");
        assert!(r.contains("2 | let y = ?"), "{r}");
        assert!(r.contains("|         ^ here"), "{r}");
    }
}
