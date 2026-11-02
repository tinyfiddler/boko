//! Positions in the source document, expressed as EPUB CFI steps.
//!
//! Optimize passes only ever narrow, extend or unlink [`TextRange`]s over
//! the chapter's append-only text buffer, so mapping *buffer bytes* back to
//! source text nodes survives every pass without the passes knowing about it.
//!
//! Step indices follow the EPUB CFI spec as foliate-js implements it: only
//! element and character-data children count, elements take even indices,
//! the text "chunk" between two elements takes the odd index between them,
//! and offsets are UTF-16 code units into the chunk.

use rustc_hash::FxHashMap;

use super::node::{NodeId, TextRange};

/// Interned path from the document element to a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PathId(u32);

impl PathId {
    /// The document element itself: the empty path.
    pub const ROOT: PathId = PathId(0);
}

#[derive(Debug, Clone)]
struct PathStep {
    parent: PathId,
    index: u32,
    id: Option<Box<str>>,
}

/// One text node's contribution to the chapter text buffer.
#[derive(Debug, Clone)]
struct SourceRun {
    buf_start: u32,
    buf_len: u32,
    chunk: PathId,
    /// UTF-16 offset of the run's first source character within its chunk.
    src_base: u32,
    /// `(buffer byte offset, source UTF-16 offset)` pairs, both relative to
    /// the run, wherever whitespace collapsing made the two drift apart.
    breaks: Box<[(u32, u32)]>,
}

/// A stretch of the text buffer that maps one-to-one, character for
/// character, onto a source text chunk.
///
/// Segments break wherever whitespace collapsing made buffer and source
/// drift apart; a collapsed whitespace run maps to its first source
/// character. They come in buffer order and never overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSegment {
    /// Byte offset of the segment's first character in the text buffer.
    pub buf_start: u32,
    /// Length of the segment in buffer bytes.
    pub buf_len: u32,
    /// The text chunk the segment comes from.
    pub chunk: PathId,
    /// UTF-16 offset of the segment's first character within its chunk.
    pub src_offset: u32,
}

/// A position resolved from a CFI: a byte offset into the chapter text
/// buffer, or an element that carries no text (an image, a rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcePosition {
    /// Byte offset into the chapter text buffer.
    Text(u32),
    /// A text-less element.
    Element(NodeId),
}

/// Maps a compiled [`Chapter`](super::Chapter) back to CFI positions in the
/// document it was compiled from.
#[derive(Debug, Clone)]
pub struct SourceMap {
    paths: Vec<PathStep>,
    runs: Vec<SourceRun>,
    elements: FxHashMap<NodeId, PathId>,
}

impl Default for SourceMap {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceMap {
    /// An empty map.
    pub fn new() -> Self {
        Self {
            paths: vec![PathStep {
                parent: PathId::ROOT,
                index: 0,
                id: None,
            }],
            runs: Vec::new(),
            elements: FxHashMap::default(),
        }
    }

    pub(crate) fn push_path(&mut self, parent: PathId, index: u32, id: Option<&str>) -> PathId {
        let path = PathId(self.paths.len() as u32);
        self.paths.push(PathStep {
            parent,
            index,
            id: id.filter(|s| !s.is_empty()).map(Into::into),
        });
        path
    }

    pub(crate) fn set_element(&mut self, node: NodeId, path: PathId) {
        self.elements.insert(node, path);
    }

    /// Record text appended to the buffer from a source text node, either
    /// verbatim or with whitespace runs collapsed to one space.
    pub(crate) fn add_run(
        &mut self,
        range: TextRange,
        chunk: PathId,
        src_base: u32,
        source: &str,
        collapsed: bool,
    ) {
        let breaks = if collapsed {
            collapse_breaks(source)
        } else {
            Box::default()
        };
        self.runs.push(SourceRun {
            buf_start: range.start,
            buf_len: range.len,
            chunk,
            src_base,
            breaks,
        });
    }

    /// Record synthetic text (a whitespace-only node kept as one space).
    pub(crate) fn add_point_run(&mut self, range: TextRange, chunk: PathId, src_base: u32) {
        self.runs.push(SourceRun {
            buf_start: range.start,
            buf_len: range.len,
            chunk,
            src_base,
            breaks: vec![(range.len, 0)].into_boxed_slice(),
        });
    }

    /// The CFI path (`/4/2[id]/6`) of an element node.
    pub fn element_path(&self, node: NodeId) -> Option<String> {
        self.elements.get(&node).map(|&p| self.path_string(p))
    }

    /// The source element an element node was compiled from.
    pub fn element(&self, node: NodeId) -> Option<PathId> {
        self.elements.get(&node).copied()
    }

    /// Every identity-mapped stretch of the text buffer, in buffer order.
    pub fn segments(&self) -> impl Iterator<Item = SourceSegment> + '_ {
        self.runs.iter().flat_map(|run| {
            let points = std::iter::once((0, 0)).chain(run.breaks.iter().copied());
            let ends = run
                .breaks
                .iter()
                .map(|&(out, _)| out)
                .chain(std::iter::once(run.buf_len));
            points.zip(ends).filter_map(move |((out, src), end)| {
                (end > out).then_some(SourceSegment {
                    buf_start: run.buf_start + out,
                    buf_len: end - out,
                    chunk: run.chunk,
                    src_offset: run.src_base + src,
                })
            })
        })
    }

    /// The step indices of a path from the document element, without id
    /// assertions.
    pub fn path_steps(&self, path: PathId) -> Vec<u32> {
        self.indices(path)
    }

    /// The CFI path with character offset (`/4/2/3:17`) of a byte offset in
    /// the chapter text buffer. `text` is the full buffer.
    pub fn text_path(&self, text: &str, buf_offset: u32) -> Option<String> {
        let run = self.run_at(buf_offset)?;
        let rel = buf_offset - run.buf_start;
        let src = run.src_base + src_offset(run, text, rel);
        Some(format!("{}:{}", self.path_string(run.chunk), src))
    }

    /// Resolve the in-document part of a CFI (everything after the `!`).
    ///
    /// Accepts a point (`/4/2/3:17`), an element (`/4/2[intro]`) or a range
    /// (`/4/2,/3:0,/5:12`, resolved to its start). A position inside content
    /// that did not survive compilation (hidden elements, dropped
    /// whitespace) snaps to the nearest following surviving text, or the
    /// last text when nothing follows.
    pub fn resolve(&self, text: &str, cfi: &str) -> Option<SourcePosition> {
        let (steps, offset) = parse_local(cfi)?;
        if let Some(path) = self.find_path(&steps)
            && let Some((&node, _)) = self.elements.iter().find(|&(_, &p)| p == path)
            && offset.is_none()
            && !self.has_text_under(path)
        {
            return Some(SourcePosition::Element(node));
        }
        let target = (
            steps.iter().map(|s| s.0).collect::<Vec<_>>(),
            offset.unwrap_or(0),
        );
        let mut best: Option<&SourceRun> = None;
        for run in &self.runs {
            let key = self.indices(run.chunk);
            let run_end = run.src_base + src_offset(run, text, run.buf_len);
            let ord = compare_position(&key, run.src_base, run_end, &target);
            match ord {
                RunOrder::Contains(src) => {
                    return Some(SourcePosition::Text(
                        run.buf_start + buf_offset(run, text, src - run.src_base),
                    ));
                }
                RunOrder::After => {
                    if best.is_none_or(|b| run.buf_start < b.buf_start) {
                        best = Some(run);
                    }
                }
                RunOrder::Before => {}
            }
        }
        if let Some(run) = best {
            return Some(SourcePosition::Text(run.buf_start));
        }
        self.runs
            .iter()
            .max_by_key(|r| r.buf_start)
            .map(|r| SourcePosition::Text(r.buf_start + r.buf_len))
    }

    fn has_text_under(&self, path: PathId) -> bool {
        self.runs.iter().any(|r| self.is_descendant(r.chunk, path))
    }

    fn is_descendant(&self, mut path: PathId, ancestor: PathId) -> bool {
        loop {
            if path == ancestor {
                return true;
            }
            if path == PathId::ROOT {
                return false;
            }
            path = self.paths[path.0 as usize].parent;
        }
    }

    fn find_path(&self, steps: &[(u32, Option<String>)]) -> Option<PathId> {
        let target: Vec<u32> = steps.iter().map(|s| s.0).collect();
        (1..self.paths.len() as u32)
            .map(PathId)
            .find(|&p| self.indices(p) == target)
    }

    fn run_at(&self, buf_offset: u32) -> Option<&SourceRun> {
        let i = self
            .runs
            .partition_point(|r| r.buf_start <= buf_offset)
            .checked_sub(1)?;
        let run = &self.runs[i];
        (buf_offset <= run.buf_start + run.buf_len).then_some(run)
    }

    fn indices(&self, mut path: PathId) -> Vec<u32> {
        let mut out = Vec::new();
        while path != PathId::ROOT {
            let step = &self.paths[path.0 as usize];
            out.push(step.index);
            path = step.parent;
        }
        out.reverse();
        out
    }

    /// The CFI string of a path (`/4/2[id]/3`), ids escaped.
    pub fn path_string(&self, mut path: PathId) -> String {
        let mut steps = Vec::new();
        while path != PathId::ROOT {
            steps.push(&self.paths[path.0 as usize]);
            path = self.paths[path.0 as usize].parent;
        }
        let mut out = String::new();
        for step in steps.iter().rev() {
            out.push('/');
            out.push_str(&step.index.to_string());
            if let Some(id) = &step.id {
                out.push('[');
                escape_into(id, &mut out);
                out.push(']');
            }
        }
        out
    }
}

/// Breakpoints after every whitespace run that collapsed to fewer source
/// characters than it spanned. Mirrors `Chapter::append_text_normalized`.
fn collapse_breaks(source: &str) -> Box<[(u32, u32)]> {
    let mut breaks = Vec::new();
    let (mut out, mut src) = (0u32, 0u32);
    let mut run_len = 0u32;
    for c in source.chars() {
        if c.is_whitespace() {
            if run_len == 0 {
                out += 1;
            }
            run_len += 1;
            src += c.len_utf16() as u32;
        } else {
            if run_len > 1 {
                breaks.push((out, src));
            }
            run_len = 0;
            out += c.len_utf8() as u32;
            src += c.len_utf16() as u32;
        }
    }
    if run_len > 1 {
        breaks.push((out, src));
    }
    breaks.into_boxed_slice()
}

fn src_offset(run: &SourceRun, text: &str, rel: u32) -> u32 {
    let (bp_out, bp_src) = run
        .breaks
        .iter()
        .take_while(|(o, _)| *o <= rel)
        .last()
        .copied()
        .unwrap_or((0, 0));
    let start = (run.buf_start + bp_out) as usize;
    let end = (run.buf_start + rel) as usize;
    let tail = text
        .get(start..end)
        .map_or(0, |s| s.encode_utf16().count() as u32);
    bp_src + tail
}

fn buf_offset(run: &SourceRun, text: &str, src_rel: u32) -> u32 {
    let (bp_out, bp_src) = run
        .breaks
        .iter()
        .take_while(|(_, s)| *s <= src_rel)
        .last()
        .copied()
        .unwrap_or((0, 0));
    let start = (run.buf_start + bp_out) as usize;
    let end = (run.buf_start + run.buf_len) as usize;
    let mut remaining = src_rel - bp_src;
    let mut out = bp_out;
    for c in text.get(start..end).unwrap_or("").chars() {
        if remaining == 0 {
            break;
        }
        remaining = remaining.saturating_sub(c.len_utf16() as u32);
        out += c.len_utf8() as u32;
    }
    out.min(run.buf_len)
}

enum RunOrder {
    Before,
    Contains(u32),
    After,
}

fn compare_position(key: &[u32], start: u32, end: u32, target: &(Vec<u32>, u32)) -> RunOrder {
    use std::cmp::Ordering::*;
    match key.cmp(&target.0) {
        Less => RunOrder::Before,
        Greater => RunOrder::After,
        Equal if target.1 < start => RunOrder::After,
        Equal if target.1 <= end => RunOrder::Contains(target.1),
        Equal => RunOrder::Before,
    }
}

fn escape_into(id: &str, out: &mut String) {
    for c in id.chars() {
        if matches!(c, '^' | '[' | ']' | '(' | ')' | ',' | ';' | '=') {
            out.push('^');
        }
        out.push(c);
    }
}

type Steps = Vec<(u32, Option<String>)>;

/// Parse the local (post-`!`) part of a CFI into steps and the terminal
/// character offset. Ranges resolve to their start: parent steps followed by
/// the start's steps.
fn parse_local(cfi: &str) -> Option<(Steps, Option<u32>)> {
    let body = cfi
        .trim()
        .strip_prefix("epubcfi(")
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(cfi.trim());
    let local = split_unescaped(body, '!').last()?.to_string();
    let parts = split_unescaped(&local, ',');
    let joined = match parts.as_slice() {
        [single] => single.clone(),
        [parent, start, ..] => format!("{parent}{start}"),
        [] => return None,
    };
    let mut steps = Vec::new();
    let mut offset = None;
    let mut chars = joined.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' => {
                let n = take_number(&mut chars)?;
                steps.push((n, None));
                offset = None;
            }
            ':' => offset = Some(take_number(&mut chars)?),
            '[' => {
                let mut id = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '^' => id.extend(chars.next()),
                        ']' => break,
                        _ => id.push(c),
                    }
                }
                if let Some(last) = steps.last_mut()
                    && offset.is_none()
                {
                    last.1 = Some(id.split(';').next().unwrap_or("").to_string());
                }
            }
            '~' | '@' => {
                while chars
                    .peek()
                    .is_some_and(|c| c.is_ascii_digit() || *c == '.' || *c == ':')
                {
                    chars.next();
                }
            }
            _ => {}
        }
    }
    (!steps.is_empty()).then_some((steps, offset))
}

fn take_number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<u32> {
    let mut n = String::new();
    while let Some(c) = chars.peek().copied().filter(char::is_ascii_digit) {
        n.push(c);
        chars.next();
    }
    n.parse().ok()
}

fn split_unescaped(s: &str, sep: char) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut escape = false;
    for c in s.chars() {
        if escape {
            escape = false;
        } else if c == '^' {
            escape = true;
        } else if c == sep {
            parts.push(String::new());
            continue;
        }
        parts.last_mut().unwrap().push(c);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_breaks_track_source_offsets() {
        let breaks = collapse_breaks("a  b\n\n  c d");
        assert_eq!(&*breaks, &[(2, 3), (4, 8)]);
    }

    #[test]
    fn parses_points_ranges_and_wrapped_forms() {
        let (steps, off) = parse_local("epubcfi(/6/14[ch1]!/4/2[p^]x]/3:10)").unwrap();
        assert_eq!(steps.iter().map(|s| s.0).collect::<Vec<_>>(), vec![4, 2, 3]);
        assert_eq!(steps[1].1.as_deref(), Some("p]x"));
        assert_eq!(off, Some(10));

        let (steps, off) = parse_local("epubcfi(/6/14!/4/2/10,/1:0,/3:45)").unwrap();
        assert_eq!(
            steps.iter().map(|s| s.0).collect::<Vec<_>>(),
            vec![4, 2, 10, 1]
        );
        assert_eq!(off, Some(0));
    }

    #[test]
    fn ids_are_escaped() {
        let mut map = SourceMap::new();
        let body = map.push_path(PathId::ROOT, 4, None);
        let p = map.push_path(body, 2, Some("a[1]"));
        assert_eq!(map.path_string(p), "/4/2[a^[1^]]");
    }
}
