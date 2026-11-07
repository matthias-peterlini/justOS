//! Source locations.

use crate::prelude::*;
use core::ops::Range;

/// A half-open byte range `[start, end)` into a source file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    #[inline]
    pub fn new(start: usize, end: usize) -> Self {
        debug_assert!(start <= end);
        Span { start: start as u32, end: end as u32 }
    }

    /// Smallest span covering both `self` and `other`.
    #[inline]
    pub fn to(self, other: Span) -> Span {
        Span { start: self.start.min(other.start), end: self.end.max(other.end) }
    }

    #[inline]
    pub fn range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }

    #[inline]
    pub fn len(self) -> usize {
        (self.end - self.start) as usize
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// A named source text plus a precomputed line index. A program built from
/// several files (`import`) keeps them all here: each added file gets its
/// own range of offsets, so a [`Span`] identifies both file and position.
#[derive(Clone, Debug)]
pub struct SourceFile {
    /// The main file first.
    parts: Vec<Part>,
}

#[derive(Clone, Debug)]
struct Part {
    name: String,
    text: String,
    base: u32,
    line_starts: Vec<u32>,
}

impl Part {
    fn new(name: String, text: String, base: u32) -> Self {
        let mut line_starts = vec![0u32];
        line_starts.extend(text.bytes().enumerate().filter(|&(_, b)| b == b'\n').map(|(i, _)| i as u32 + 1));
        Part { name, text, base, line_starts }
    }

    fn line_of(&self, offset: u32) -> u32 {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i as u32 + 1,
            Err(i) => i as u32,
        }
    }

    fn line_text(&self, line: u32) -> &str {
        let idx = line.saturating_sub(1) as usize;
        let Some(&start) = self.line_starts.get(idx) else { return "" };
        let end = self.line_starts.get(idx + 1).map_or(self.text.len(), |&e| e as usize);
        self.text[start as usize..end].trim_end_matches(['\n', '\r'])
    }
}

impl SourceFile {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        SourceFile { parts: vec![Part::new(name.into(), text.into(), 0)] }
    }

    /// Add another file; returns the offset its spans start at.
    pub fn add(&mut self, name: impl Into<String>, text: impl Into<String>) -> u32 {
        let last = self.parts.last().expect("at least the main file");
        // One spare byte so an end-of-file span never touches the next file.
        let base = last.base + last.text.len() as u32 + 1;
        self.parts.push(Part::new(name.into(), text.into(), base));
        base
    }

    /// Name of the main file.
    pub fn name(&self) -> &str {
        &self.parts[0].name
    }

    /// Text of the main file.
    pub fn text(&self) -> &str {
        &self.parts[0].text
    }

    fn part(&self, offset: u32) -> &Part {
        let i = self.parts.partition_point(|p| p.base <= offset);
        &self.parts[i.saturating_sub(1)]
    }

    /// Name of the file containing `offset`.
    pub fn name_at(&self, offset: u32) -> &str {
        &self.part(offset).name
    }

    /// 1-based line number containing byte `offset`, within its file.
    pub fn line_of(&self, offset: u32) -> u32 {
        let p = self.part(offset);
        p.line_of(offset - p.base)
    }

    /// 1-based `(line, column)` within the file containing `offset`; the
    /// column counts characters, not bytes.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let p = self.part(offset);
        let local = offset - p.base;
        let line = p.line_of(local);
        let start = p.line_starts[line as usize - 1] as usize;
        let end = (local as usize).min(p.text.len());
        let col = p.text.get(start..end).map_or(0, |s| s.chars().count());
        (line, col as u32 + 1)
    }

    /// Text of a 1-based line of the main file, without its newline.
    pub fn line_text(&self, line: u32) -> &str {
        self.parts[0].line_text(line)
    }

    /// Text of the line containing `offset`, in whichever file holds it.
    pub fn line_text_at(&self, offset: u32) -> &str {
        let p = self.part(offset);
        p.line_text(p.line_of(offset - p.base))
    }

    /// Number of lines of the main file.
    pub fn line_count(&self) -> usize {
        self.parts[0].line_starts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_lookup() {
        let f = SourceFile::new("t", "ab\ncàd\n\nx");
        assert_eq!(f.line_col(0), (1, 1));
        assert_eq!(f.line_col(3), (2, 1));
        assert_eq!(f.line_col(6), (2, 3)); // after the two-byte 'à'
        assert_eq!(f.line_text(2), "càd");
        assert_eq!(f.line_text(3), "");
        assert_eq!(f.line_col(9), (4, 1));
    }

    #[test]
    fn several_files() {
        let mut f = SourceFile::new("main.jt", "a\nb\n");
        let base = f.add("utils.jt", "x\ny\nz");
        assert_eq!(base, 5);
        assert_eq!((f.name_at(2), f.line_col(2)), ("main.jt", (2, 1)));
        assert_eq!((f.name_at(base + 4), f.line_col(base + 4)), ("utils.jt", (3, 1)));
        assert_eq!(f.line_text_at(base + 2), "y");
        assert_eq!(f.name(), "main.jt");
    }
}
