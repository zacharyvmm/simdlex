//! YAML stage 2: a streaming state machine over the stage-1 masks.
//!
//! Block structure is processed one line at a time. For each line, the
//! indentation (one `run_length` over the space mask) decides which open
//! block collections close, whether the line opens a nested node, or whether
//! it continues the current collection. The line break mask bounds each line,
//! and the parser reads `:`, `#`, `-`, and flow indicators in place within
//! those bounds. Quoted scalars, which may span lines, end at the next
//! matching quote on the tape. Flow collections use a nested state machine
//! on the same frame stack.

use std::cell::Cell;

use simdlex_index::MaskView;

use super::{
    LINE_BREAK, SPACE, YamlBlockScalar, YamlChomping, YamlCollectionStyle, YamlError,
    YamlErrorKind, YamlEvent, YamlProperties, YamlScalar, YamlScalarStyle, error,
};
use crate::grammar::span;

const LINE_BREAK_BIT: u8 = 1 << LINE_BREAK;

/// Quote offsets: a tape in [`simdlex_index::IndexMode::Hybrid`], the
/// structural mask in [`simdlex_index::IndexMode::Masks`].
pub(crate) enum Quotes<'m> {
    Tape {
        tape: &'m [u32],
        /// Tape index of the last match; searches gallop forward from it.
        hint: Cell<usize>,
    },
    Masks(MaskView<'m>),
}

impl<'m> Quotes<'m> {
    pub(crate) fn tape(tape: &'m [u32]) -> Self {
        Self::Tape {
            tape,
            hint: Cell::new(0),
        }
    }

    /// The first quote in `[from, limit)` whose byte satisfies `wanted`.
    #[inline]
    fn find(
        &self,
        input: &[u8],
        from: usize,
        limit: usize,
        wanted: impl Fn(u8) -> bool,
    ) -> Option<usize> {
        match self {
            Self::Tape { tape, hint } => {
                let mut index = seek(tape, hint.get(), from);
                while let Some(&offset) = tape.get(index) {
                    let offset = offset as usize;
                    if offset >= limit {
                        break;
                    }
                    if wanted(input[offset]) {
                        hint.set(index);
                        return Some(offset);
                    }
                    index += 1;
                }
                hint.set(index);
                None
            }
            Self::Masks(masks) => {
                let mut from = from;
                while let Some(offset) = masks.next_structural(from) {
                    if offset >= limit {
                        return None;
                    }
                    if wanted(input[offset]) {
                        return Some(offset);
                    }
                    from = offset + 1;
                }
                None
            }
        }
    }
}

/// Index of the first tape entry at or after `from`. Searches mostly move
/// forward a little at a time, so gallop from `hint` before falling back to
/// binary search.
#[inline]
fn seek(tape: &[u32], hint: usize, from: usize) -> usize {
    let before = |index: usize| (tape[index] as usize) < from;
    if hint > tape.len() || (hint > 0 && !before(hint - 1)) {
        return tape.partition_point(|&offset| (offset as usize) < from);
    }
    let mut index = hint;
    for _ in 0..8 {
        if index >= tape.len() || !before(index) {
            return index;
        }
        index += 1;
    }
    index + tape[index..].partition_point(|&offset| (offset as usize) < from)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    BlockMap,
    BlockSeq,
    FlowMap,
    FlowSeq,
}

/// Progress through a flow collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Expecting an entry (a key, in mappings).
    Entry,
    /// After an entry (a key, in mappings).
    AfterEntry,
    /// After `:` in a mapping.
    Value,
    /// After a mapping value.
    AfterValue,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Frame {
    kind: Kind,
    /// Column of a block collection's entries; unused for flow collections.
    indent: i64,
    flow: Flow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parent {
    Root,
    Map,
    Seq,
}

/// A node that has been announced (by `key:`, `-`, or a document start) but
/// whose content has not been seen yet.
#[derive(Debug, Clone, Copy)]
struct Pending<'a> {
    /// Indentation of the owning collection; the node must be indented
    /// further (or, for a sequence under a mapping key, equally).
    parent_indent: i64,
    parent: Parent,
    properties: YamlProperties<'a>,
}

pub(crate) struct Stage2<'a, 'm, 'f, F> {
    input: &'a [u8],
    masks: MaskView<'m>,
    quotes: Quotes<'m>,
    frames: &'f mut Vec<Frame>,
    max_depth: usize,
    sink: F,
    pending: Option<Pending<'a>>,
    document_open: bool,
    /// Whether the last completed flow node is a quoted scalar or flow
    /// collection: a `:` after it is a value indicator even without a
    /// following space (`{"a":1}`, `{"a" :1}`).
    adjacent_value: bool,
}

impl<'a, 'm, 'f, F> Stage2<'a, 'm, 'f, F>
where
    F: FnMut(YamlEvent<'a>),
{
    pub(crate) fn new(
        input: &'a [u8],
        masks: MaskView<'m>,
        quotes: Quotes<'m>,
        frames: &'f mut Vec<Frame>,
        max_depth: usize,
        sink: F,
    ) -> Self {
        Self {
            input,
            masks,
            quotes,
            frames,
            max_depth,
            sink,
            pending: None,
            document_open: false,
            adjacent_value: false,
        }
    }

    pub(crate) fn run(mut self) -> Result<(), YamlError> {
        let len = self.input.len();
        let mut line = if self.input.starts_with("\u{feff}".as_bytes()) {
            3
        } else {
            0
        };

        while line < len {
            let indent = self.masks.run_length(SPACE, line);
            let content = line + indent;
            let first = self.skip_blanks(content);
            if self.at_line_end(first) {
                line = self.next_line(first);
                continue;
            }

            if line == content {
                if self.is_document_marker(line, b'-') {
                    self.start_document(line, true)?;
                    let after = self.skip_blanks(line + 3);
                    line = if self.at_line_end(after) {
                        self.next_line(after)
                    } else {
                        self.block_line(line, after)?
                    };
                    continue;
                }
                if self.is_document_marker(line, b'.') {
                    self.end_document(line, true);
                    line = self.end_of_line(line + 3)?;
                    continue;
                }
                if self.input[line] == b'%' && !self.document_open {
                    // Directives are skipped; tag handles are not resolved.
                    line = self.next_line(line);
                    continue;
                }
            }

            if first != content {
                return Err(error(YamlErrorKind::TabIndentation, content));
            }
            if !self.document_open {
                self.start_document(content, false)?;
            }
            line = self.block_line(line, content)?;
        }

        self.end_document(len, false);
        Ok(())
    }

    // ----- documents -----

    fn start_document(&mut self, offset: usize, explicit: bool) -> Result<(), YamlError> {
        self.end_document(offset, false);
        self.emit(YamlEvent::DocumentStart {
            explicit,
            offset: offset as u32,
        });
        self.document_open = true;
        self.pending = Some(Pending {
            parent_indent: -1,
            parent: Parent::Root,
            properties: YamlProperties::default(),
        });
        Ok(())
    }

    fn end_document(&mut self, offset: usize, explicit: bool) {
        if !self.document_open {
            return;
        }
        self.resolve_empty(offset);
        while let Some(frame) = self.frames.pop() {
            self.emit_end(frame.kind, offset);
        }
        self.emit(YamlEvent::DocumentEnd {
            explicit,
            offset: offset as u32,
        });
        self.document_open = false;
    }

    // ----- block context -----

    /// Process the content of one line, starting at `at`. Returns the start
    /// of the next line to process.
    fn block_line(&mut self, line: usize, mut at: usize) -> Result<usize, YamlError> {
        loop {
            let col = (at - line) as i64;
            let entry = self.is_entry(at);
            let nested = match self.pending {
                Some(pending) => {
                    col > pending.parent_indent
                        || (entry && pending.parent == Parent::Map && col == pending.parent_indent)
                }
                None => false,
            };

            if !nested {
                self.resolve_empty(at);
                self.close_blocks(at, col, entry);
                match self.frames.last() {
                    Some(top) if top.indent == col => {}
                    Some(_) => return Err(error(YamlErrorKind::BadIndentation, at)),
                    None => return Err(error(YamlErrorKind::UnexpectedContent, at)),
                }
            }

            if entry {
                if nested {
                    let pending = self.pending.take().unwrap();
                    self.open_block(Kind::BlockSeq, col, pending.properties, at)?;
                } else if self.top_kind() != Some(Kind::BlockSeq) {
                    return Err(error(YamlErrorKind::UnexpectedContent, at));
                }
                self.pending = Some(Pending {
                    parent_indent: col,
                    parent: Parent::Seq,
                    properties: YamlProperties::default(),
                });
                let after = self.skip_blanks(at + 1);
                if self.at_line_end(after) {
                    return Ok(self.next_line(after));
                }
                // A compact node on the same line: `- a`, `- k: v`, `- - a`.
                at = after;
                continue;
            }

            let (properties, after) = self.properties(at, false)?;
            if !properties.is_empty() && self.at_line_end(after) {
                // Properties of a node that starts on a later line.
                if !nested {
                    return Err(error(YamlErrorKind::UnexpectedContent, at));
                }
                let pending = self.pending.as_mut().unwrap();
                pending.properties = merge(pending.properties, properties, at)?;
                return Ok(self.next_line(after));
            }
            if matches!(self.input[after], b'?' | b':') && self.ends_token(after + 1) {
                return Err(error(YamlErrorKind::Unsupported, after));
            }

            if let Some((key, colon)) = self.implicit_key(after, properties)? {
                if nested {
                    let pending = self.pending.take().unwrap();
                    self.open_block(Kind::BlockMap, col, pending.properties, at)?;
                } else if self.top_kind() != Some(Kind::BlockMap) {
                    return Err(error(YamlErrorKind::UnexpectedContent, at));
                }
                self.emit(key);
                self.pending = Some(Pending {
                    parent_indent: col,
                    parent: Parent::Map,
                    properties: YamlProperties::default(),
                });
                let value = self.skip_blanks(colon + 1);
                let (properties, value) = self.properties(value, false)?;
                self.pending.as_mut().unwrap().properties = properties;
                if self.at_line_end(value) {
                    return Ok(self.next_line(value));
                }
                return self.inline_value(value);
            }

            if !nested {
                return Err(error(YamlErrorKind::UnexpectedContent, at));
            }
            let pending = self.pending.as_mut().unwrap();
            pending.properties = merge(pending.properties, properties, at)?;
            return self.inline_value(after);
        }
    }

    /// Close block collections that the content at `col` dedents out of.
    fn close_blocks(&mut self, at: usize, col: i64, entry: bool) {
        while let Some(&top) = self.frames.last() {
            // A sequence may share its parent mapping's indentation
            // (`key:\n- a`); a non-entry line at that column ends it.
            let close =
                top.indent > col || (top.indent == col && top.kind == Kind::BlockSeq && !entry);
            if !close {
                break;
            }
            self.frames.pop();
            self.emit_end(top.kind, at);
        }
    }

    /// A node that starts on the current line after `key: ` or `- `, or at
    /// the root. Consumes [`Self::pending`].
    fn inline_value(&mut self, at: usize) -> Result<usize, YamlError> {
        let pending = self.pending.take().unwrap();
        let properties = pending.properties;
        let input = self.input;
        match input[at] {
            b'*' => {
                if !properties.is_empty() {
                    return Err(error(YamlErrorKind::UnexpectedContent, at));
                }
                let (alias, end) = self.alias(at)?;
                self.emit(alias);
                self.end_of_line(end)
            }
            b'"' => {
                let close = self.double_quote_end(at)?;
                self.scalar(at + 1, close, YamlScalarStyle::DoubleQuoted, properties);
                self.end_of_line(close + 1)
            }
            b'\'' => {
                let close = self.single_quote_end(at)?;
                self.scalar(at + 1, close, YamlScalarStyle::SingleQuoted, properties);
                self.end_of_line(close + 1)
            }
            b'|' | b'>' => self.block_scalar(at, pending.parent_indent, properties),
            b'[' | b'{' => {
                let end = self.flow_collection(at, properties)?;
                self.end_of_line(end)
            }
            _ if self.is_entry(at) => Err(error(YamlErrorKind::UnexpectedContent, at)),
            b'?' | b':' if self.ends_token(at + 1) => Err(error(YamlErrorKind::Unsupported, at)),
            _ if !self.plain_start(at, false) => Err(error(YamlErrorKind::InvalidPlainScalar, at)),
            _ => self.plain_block(at, pending.parent_indent, properties),
        }
    }

    /// If a single-line implicit key starts at `at`, returns its event and
    /// the offset of its `:`.
    fn implicit_key(
        &self,
        at: usize,
        properties: YamlProperties<'a>,
    ) -> Result<Option<(YamlEvent<'a>, usize)>, YamlError> {
        let input = self.input;
        let line_end = self.line_end(at);
        let key = |start, end, style| {
            YamlEvent::Scalar(YamlScalar {
                properties,
                ..self.make_scalar(start, end, style)
            })
        };
        let quoted = match input[at] {
            b'"' => Some((self.double_quote_end(at), YamlScalarStyle::DoubleQuoted)),
            b'\'' => Some((self.single_quote_end(at), YamlScalarStyle::SingleQuoted)),
            b'*' => return self.alias_key(at, properties),
            _ => None,
        };
        if let Some((close, style)) = quoted {
            // Unterminated or multi-line quotes cannot be keys; the value
            // path reports any error.
            let Ok(close) = close else {
                return Ok(None);
            };
            if close > line_end {
                return Ok(None);
            }
            let colon = self.skip_blanks(close + 1);
            if input.get(colon) == Some(&b':') && self.ends_token(colon + 1) {
                return Ok(Some((key(at + 1, close, style), colon)));
            }
            return Ok(None);
        }

        if !self.plain_start(at, false) {
            return Ok(None);
        }
        for found in self.colons_and_hashes(at, line_end) {
            if input[found] == b'#' {
                if is_blank(input[found - 1]) {
                    return Ok(None);
                }
            } else if self.ends_token(found + 1) {
                let end = self.trim_end(at, found);
                return Ok(Some((key(at, end, YamlScalarStyle::Plain), found)));
            }
        }
        Ok(None)
    }

    /// An alias used as an implicit key: `*a : b`. Anchor names may contain
    /// `:`, so the indicator must be separated from the name.
    fn alias_key(
        &self,
        at: usize,
        properties: YamlProperties<'a>,
    ) -> Result<Option<(YamlEvent<'a>, usize)>, YamlError> {
        let (alias, end) = self.alias(at)?;
        let colon = self.skip_blanks(end);
        if self.input.get(colon) != Some(&b':') || !self.ends_token(colon + 1) {
            return Ok(None);
        }
        if !properties.is_empty() {
            return Err(error(YamlErrorKind::UnexpectedContent, at));
        }
        Ok(Some((alias, colon)))
    }

    /// The alias `*name` at `at`, and the offset after it.
    fn alias(&self, at: usize) -> Result<(YamlEvent<'a>, usize), YamlError> {
        let end = self.token_end(at + 1);
        if end == at + 1 {
            return Err(error(YamlErrorKind::UnexpectedContent, at));
        }
        let alias = YamlEvent::Alias {
            name: &self.input[at + 1..end],
            span: span(at + 1, end),
        };
        Ok((alias, end))
    }

    /// A plain scalar in block context, possibly continued on more-indented
    /// lines. Returns the start of the next line.
    fn plain_block(
        &mut self,
        at: usize,
        parent_indent: i64,
        properties: YamlProperties<'a>,
    ) -> Result<usize, YamlError> {
        let len = self.input.len();
        let (mut end, mut comment) = self.plain_line(at)?;
        let mut next = self.next_line(end);
        let mut line = next;

        while !comment && line < len {
            let indent = self.masks.run_length(SPACE, line);
            let first = self.skip_blanks(line + indent);
            if first >= len {
                break;
            }
            if is_break(self.input[first]) {
                line = self.next_line(first);
                continue;
            }
            if indent as i64 <= parent_indent
                || self.input[first] == b'#'
                || (indent == 0
                    && (self.is_document_marker(line, b'-') || self.is_document_marker(line, b'.')))
            {
                break;
            }
            (end, comment) = self.plain_line(first)?;
            next = self.next_line(end);
            line = next;
        }

        self.scalar(at, end, YamlScalarStyle::Plain, properties);
        Ok(next)
    }

    /// The end of plain text on one line, and whether a comment follows.
    fn plain_line(&self, at: usize) -> Result<(usize, bool), YamlError> {
        let input = self.input;
        let line_end = self.line_end(at);
        for found in self.colons_and_hashes(at, line_end) {
            if input[found] == b'#' {
                if found > at && is_blank(input[found - 1]) {
                    return Ok((self.trim_end(at, found), true));
                }
            } else if self.ends_token(found + 1) {
                // `key: a: b` or a continuation line that looks like a key.
                return Err(error(YamlErrorKind::UnexpectedContent, found));
            }
        }
        Ok((self.trim_end(at, line_end), false))
    }

    /// Offsets of `:` and `#` in `[from, limit)`, found by scanning the
    /// line.
    fn colons_and_hashes(&self, from: usize, limit: usize) -> impl Iterator<Item = usize> + '_ {
        self.input[from..limit]
            .iter()
            .enumerate()
            .filter(|&(_, &byte)| byte == b':' || byte == b'#')
            .map(move |(offset, _)| from + offset)
    }

    /// A `|` or `>` block scalar. Returns the start of the line after it.
    fn block_scalar(
        &mut self,
        at: usize,
        parent_indent: i64,
        properties: YamlProperties<'a>,
    ) -> Result<usize, YamlError> {
        let input = self.input;
        let len = input.len();
        let literal = input[at] == b'|';

        let mut chomping = None;
        let mut explicit = None;
        let mut header = at + 1;
        while header < len {
            match input[header] {
                b'+' if chomping.is_none() => chomping = Some(YamlChomping::Keep),
                b'-' if chomping.is_none() => chomping = Some(YamlChomping::Strip),
                digit @ b'1'..=b'9' if explicit.is_none() => explicit = Some((digit - b'0') as i64),
                _ => break,
            }
            header += 1;
        }
        if !self.ends_token(header) {
            return Err(error(YamlErrorKind::InvalidBlockScalarHeader, header));
        }
        let content = self
            .end_of_line(header)
            .map_err(|_| error(YamlErrorKind::InvalidBlockScalarHeader, header))?;

        let minimum = parent_indent + 1;
        let indent = match explicit {
            Some(digits) if parent_indent >= 0 => parent_indent + digits,
            Some(digits) => digits,
            None => {
                // The first non-empty line sets the indentation; with none,
                // the longest leading empty line does.
                let mut line = content;
                let mut longest_empty = 0;
                let mut first_content = None;
                while line < len {
                    let spaces = self.masks.run_length(SPACE, line);
                    let first = line + spaces;
                    if first < len && !is_break(input[first]) {
                        first_content = Some((spaces as i64, line));
                        break;
                    }
                    longest_empty = longest_empty.max(spaces as i64);
                    line = self.next_line(first);
                }
                match first_content {
                    Some((spaces, line)) if spaces >= minimum => {
                        if longest_empty > spaces {
                            return Err(error(YamlErrorKind::BadIndentation, line));
                        }
                        spaces
                    }
                    _ => minimum.max(longest_empty),
                }
            }
        };

        let mut line = content;
        let mut end = content;
        while line < len {
            let spaces = self.masks.run_length(SPACE, line);
            let first = line + spaces;
            let blank = first >= len || is_break(input[first]);
            if !blank
                && ((spaces as i64) < indent
                    || (spaces == 0
                        && (self.is_document_marker(line, b'-')
                            || self.is_document_marker(line, b'.'))))
            {
                break;
            }
            line = self.next_line(first);
            end = line;
        }

        let header = YamlBlockScalar {
            indent: indent as u32,
            chomping: chomping.unwrap_or(YamlChomping::Clip),
        };
        let style = if literal {
            YamlScalarStyle::Literal(header)
        } else {
            YamlScalarStyle::Folded(header)
        };
        self.scalar(content, end, style, properties);
        Ok(end)
    }

    // ----- flow context -----

    /// A flow collection starting at `open`. Returns the offset after its
    /// closing bracket.
    fn flow_collection(
        &mut self,
        open: usize,
        properties: YamlProperties<'a>,
    ) -> Result<usize, YamlError> {
        let input = self.input;
        let base = self.frames.len();
        self.open_flow(open, properties)?;
        let mut at = open + 1;
        let mut next_break = self.line_end(at);

        while self.frames.len() > base {
            at = self.skip_flow_space(at);
            next_break = self
                .breaks_before(next_break, at)
                .ok_or_else(|| error(YamlErrorKind::UnterminatedFlow, open))?;
            let Some(&byte) = input.get(at) else {
                return Err(error(YamlErrorKind::UnterminatedFlow, open));
            };
            let top = *self.frames.last().unwrap();
            let is_map = top.kind == Kind::FlowMap;
            match byte {
                b']' | b'}' => {
                    let expected = if byte == b']' {
                        Kind::FlowSeq
                    } else {
                        Kind::FlowMap
                    };
                    if top.kind != expected {
                        return Err(error(YamlErrorKind::UnexpectedContent, at));
                    }
                    if is_map && matches!(top.flow, Flow::AfterEntry | Flow::Value) {
                        self.empty_scalar(at, YamlProperties::default());
                    }
                    self.frames.pop();
                    self.emit_end(top.kind, at);
                    at += 1;
                    self.flow_node_done(true);
                }
                b',' => {
                    match top.flow {
                        Flow::Entry => return Err(error(YamlErrorKind::UnexpectedContent, at)),
                        Flow::AfterEntry if is_map => {
                            self.empty_scalar(at, YamlProperties::default())
                        }
                        Flow::Value => self.empty_scalar(at, YamlProperties::default()),
                        Flow::AfterEntry | Flow::AfterValue => {}
                    }
                    self.frames.last_mut().unwrap().flow = Flow::Entry;
                    at += 1;
                }
                b':' if top.flow == Flow::AfterEntry
                    && (self.ends_flow_token(at + 1) || self.adjacent_value) =>
                {
                    if !is_map {
                        // `[a: b]`: a single-pair mapping.
                        return Err(error(YamlErrorKind::Unsupported, at));
                    }
                    self.frames.last_mut().unwrap().flow = Flow::Value;
                    at += 1;
                }
                b':' if top.flow == Flow::Entry && self.ends_flow_token(at + 1) => {
                    return Err(error(YamlErrorKind::Unsupported, at));
                }
                _ => {
                    if !matches!(top.flow, Flow::Entry | Flow::Value) {
                        return Err(error(YamlErrorKind::UnexpectedContent, at));
                    }
                    at = self.flow_node(at)?;
                }
            }
        }
        Ok(at)
    }

    /// One node inside a flow collection. Returns the offset after it.
    fn flow_node(&mut self, at: usize) -> Result<usize, YamlError> {
        let input = self.input;
        let (properties, at) = self.properties(at, true)?;
        let Some(&byte) = input.get(at) else {
            return Err(error(YamlErrorKind::UnterminatedFlow, at));
        };
        match byte {
            b',' | b']' | b'}' => {
                self.empty_scalar(at, properties);
                self.flow_node_done(false);
                Ok(at)
            }
            b'[' | b'{' => {
                self.open_flow(at, properties)?;
                Ok(at + 1)
            }
            b'*' => {
                if !properties.is_empty() {
                    return Err(error(YamlErrorKind::UnexpectedContent, at));
                }
                let (alias, end) = self.alias(at)?;
                self.emit(alias);
                self.flow_node_done(false);
                Ok(end)
            }
            b'"' | b'\'' => {
                let (close, style) = if byte == b'"' {
                    (self.double_quote_end(at)?, YamlScalarStyle::DoubleQuoted)
                } else {
                    (self.single_quote_end(at)?, YamlScalarStyle::SingleQuoted)
                };
                self.scalar(at + 1, close, style, properties);
                self.flow_node_done(true);
                Ok(close + 1)
            }
            b'?' | b':' if self.ends_flow_token(at + 1) => {
                Err(error(YamlErrorKind::Unsupported, at))
            }
            _ if !self.plain_start(at, true) => Err(error(YamlErrorKind::InvalidPlainScalar, at)),
            _ => {
                let end = self.plain_flow_end(at);
                self.scalar(at, end, YamlScalarStyle::Plain, properties);
                self.flow_node_done(false);
                Ok(end)
            }
        }
    }

    /// The end of a plain scalar in flow context, which may span lines.
    fn plain_flow_end(&self, at: usize) -> usize {
        let input = self.input;
        for end in at..input.len() {
            let ends = match input[end] {
                b':' => self.ends_flow_token(end + 1),
                b'#' => is_blank_or_break(input[end - 1]),
                byte => is_flow_indicator(byte),
            };
            if ends {
                return self.trim_end(at, end);
            }
        }
        self.trim_end(at, input.len())
    }

    fn open_flow(&mut self, at: usize, properties: YamlProperties<'a>) -> Result<(), YamlError> {
        let kind = if self.input[at] == b'[' {
            Kind::FlowSeq
        } else {
            Kind::FlowMap
        };
        self.push(kind, -1, at)?;
        self.emit_start(kind, YamlCollectionStyle::Flow, properties, at);
        Ok(())
    }

    /// Advance the enclosing flow collection past a completed node. `json`
    /// marks a quoted scalar or flow collection, which a `:` may follow
    /// directly.
    fn flow_node_done(&mut self, json: bool) {
        self.adjacent_value = json;
        if let Some(top) = self.frames.last_mut()
            && matches!(top.kind, Kind::FlowMap | Kind::FlowSeq)
        {
            top.flow = match top.flow {
                Flow::Entry => Flow::AfterEntry,
                Flow::Value => Flow::AfterValue,
                other => other,
            };
        }
    }

    /// Skip blanks, line breaks, and comments between flow tokens.
    fn skip_flow_space(&self, mut at: usize) -> usize {
        let input = self.input;
        loop {
            while at < input.len() && is_blank_or_break(input[at]) {
                at += 1;
            }
            if at < input.len() && input[at] == b'#' && is_blank_or_break(input[at - 1]) {
                at = self.line_end(at);
                continue;
            }
            return at;
        }
    }

    // ----- scalars and properties -----

    fn double_quote_end(&self, open: usize) -> Result<usize, YamlError> {
        let input = self.input;
        let mut from = open + 1;
        loop {
            let Some(quote) = self
                .quotes
                .find(input, from, input.len(), |byte| byte == b'"')
            else {
                return Err(error(YamlErrorKind::UnterminatedQuote, open));
            };
            let mut start = quote;
            while start > open + 1 && input[start - 1] == b'\\' {
                start -= 1;
            }
            if (quote - start) % 2 == 0 {
                return self.quote_in_document(open, quote);
            }
            from = quote + 1;
        }
    }

    fn single_quote_end(&self, open: usize) -> Result<usize, YamlError> {
        let mut from = open + 1;
        loop {
            let Some(quote) = self
                .quotes
                .find(self.input, from, self.input.len(), |byte| byte == b'\'')
            else {
                return Err(error(YamlErrorKind::UnterminatedQuote, open));
            };
            if self.input.get(quote + 1) == Some(&b'\'') {
                from = quote + 2;
                continue;
            }
            return self.quote_in_document(open, quote);
        }
    }

    /// `close`, unless a document marker line lies between the quotes.
    fn quote_in_document(&self, open: usize, close: usize) -> Result<usize, YamlError> {
        match self.breaks_before(self.line_end(open), close) {
            Some(_) => Ok(close),
            None => Err(error(YamlErrorKind::UnterminatedQuote, open)),
        }
    }

    /// Optional `&anchor` and `!tag`, in either order. In flow context they
    /// may be separated by line breaks and comments.
    fn properties(
        &self,
        mut at: usize,
        flow: bool,
    ) -> Result<(YamlProperties<'a>, usize), YamlError> {
        let input = self.input;
        let separate = |at| {
            if flow {
                self.skip_flow_space(at)
            } else {
                self.skip_blanks(at)
            }
        };
        let mut properties = YamlProperties::default();
        loop {
            match input.get(at) {
                Some(b'&') if properties.anchor.is_none() => {
                    let end = self.token_end(at + 1);
                    if end == at + 1 {
                        return Err(error(YamlErrorKind::UnexpectedContent, at));
                    }
                    properties.anchor = Some(&input[at + 1..end]);
                    at = separate(end);
                }
                Some(b'!') if properties.tag.is_none() => {
                    let end = if input.get(at + 1) == Some(&b'<') {
                        self.verbatim_tag_end(at)?
                    } else {
                        self.token_end(at)
                    };
                    properties.tag = Some(&input[at..end]);
                    at = separate(end);
                }
                Some(b'&' | b'!') => return Err(error(YamlErrorKind::UnexpectedContent, at)),
                _ => return Ok((properties, at)),
            }
        }
    }

    /// End of a verbatim tag such as `!<tag:yaml.org,2002:str>`, whose URI
    /// may contain flow indicators, so it ends only at its `>`.
    fn verbatim_tag_end(&self, at: usize) -> Result<usize, YamlError> {
        let input = self.input;
        let uri = at + 2;
        let close = input[uri..]
            .iter()
            .position(|&byte| byte == b'>' || is_blank_or_break(byte))
            .map(|offset| uri + offset);
        match close {
            Some(close)
                if close > uri && input[close] == b'>' && self.ends_flow_token(close + 1) =>
            {
                Ok(close + 1)
            }
            _ => Err(error(YamlErrorKind::UnexpectedContent, at)),
        }
    }

    /// Whether a plain scalar may start at `at`.
    fn plain_start(&self, at: usize, flow: bool) -> bool {
        let safe_next = |next: usize| match self.input.get(next) {
            None => false,
            Some(&byte) => !(is_blank_or_break(byte) || (flow && is_flow_indicator(byte))),
        };
        match self.input[at] {
            b'-' | b'?' | b':' => safe_next(at + 1),
            b',' | b'[' | b']' | b'{' | b'}' | b'#' | b'&' | b'*' | b'!' | b'|' | b'>' | b'\''
            | b'"' | b'%' | b'@' | b'`' => false,
            _ => true,
        }
    }

    // ----- events -----

    fn emit(&mut self, event: YamlEvent<'a>) {
        (self.sink)(event);
    }

    fn make_scalar(&self, start: usize, end: usize, style: YamlScalarStyle) -> YamlScalar<'a> {
        YamlScalar {
            raw: &self.input[start..end],
            span: span(start, end),
            style,
            properties: YamlProperties::default(),
        }
    }

    fn scalar(
        &mut self,
        start: usize,
        end: usize,
        style: YamlScalarStyle,
        properties: YamlProperties<'a>,
    ) {
        let scalar = YamlScalar {
            properties,
            ..self.make_scalar(start, end, style)
        };
        self.emit(YamlEvent::Scalar(scalar));
    }

    fn empty_scalar(&mut self, at: usize, properties: YamlProperties<'a>) {
        self.scalar(at, at, YamlScalarStyle::Plain, properties);
    }

    /// Emit an empty node for an announced node that never appeared.
    fn resolve_empty(&mut self, at: usize) {
        if let Some(pending) = self.pending.take() {
            self.empty_scalar(at, pending.properties);
        }
    }

    fn push(&mut self, kind: Kind, indent: i64, at: usize) -> Result<(), YamlError> {
        if self.frames.len() >= self.max_depth {
            return Err(error(YamlErrorKind::Depth, at));
        }
        self.frames.push(Frame {
            kind,
            indent,
            flow: Flow::Entry,
        });
        Ok(())
    }

    fn open_block(
        &mut self,
        kind: Kind,
        indent: i64,
        properties: YamlProperties<'a>,
        at: usize,
    ) -> Result<(), YamlError> {
        self.push(kind, indent, at)?;
        self.emit_start(kind, YamlCollectionStyle::Block, properties, at);
        Ok(())
    }

    fn emit_start(
        &mut self,
        kind: Kind,
        style: YamlCollectionStyle,
        properties: YamlProperties<'a>,
        at: usize,
    ) {
        let offset = at as u32;
        self.emit(match kind {
            Kind::BlockMap | Kind::FlowMap => YamlEvent::MappingStart {
                style,
                properties,
                offset,
            },
            Kind::BlockSeq | Kind::FlowSeq => YamlEvent::SequenceStart {
                style,
                properties,
                offset,
            },
        });
    }

    fn emit_end(&mut self, kind: Kind, at: usize) {
        let offset = at as u32;
        self.emit(match kind {
            Kind::BlockMap | Kind::FlowMap => YamlEvent::MappingEnd { offset },
            Kind::BlockSeq | Kind::FlowSeq => YamlEvent::SequenceEnd { offset },
        });
    }

    #[inline]
    fn top_kind(&self) -> Option<Kind> {
        self.frames.last().map(|frame| frame.kind)
    }

    // ----- lexical helpers -----

    /// Offset of the line break ending the line containing `at`, or the end
    /// of input.
    #[inline]
    fn line_end(&self, at: usize) -> usize {
        self.masks
            .next_in(LINE_BREAK_BIT, at)
            .unwrap_or(self.input.len())
    }

    /// Start of the line after the one containing `at`. A CRLF pair is one
    /// line break.
    #[inline]
    fn next_line(&self, at: usize) -> usize {
        match self.masks.next_in(LINE_BREAK_BIT, at) {
            Some(cr) if self.input[cr] == b'\r' && self.input.get(cr + 1) == Some(&b'\n') => cr + 2,
            Some(feed) => feed + 1,
            None => self.input.len(),
        }
    }

    /// After a complete node, only blanks and a comment may follow on the
    /// line. Returns the start of the next line.
    fn end_of_line(&self, at: usize) -> Result<usize, YamlError> {
        let after = self.skip_blanks(at);
        if self.at_line_end(after) {
            Ok(self.next_line(after))
        } else {
            Err(error(YamlErrorKind::UnexpectedContent, after))
        }
    }

    /// End of input, a line break, or a comment.
    #[inline]
    fn at_line_end(&self, at: usize) -> bool {
        match self.input.get(at) {
            None => true,
            Some(&byte) if is_break(byte) => true,
            Some(b'#') => at == 0 || is_blank_or_break(self.input[at - 1]),
            Some(_) => false,
        }
    }

    #[inline]
    fn skip_blanks(&self, mut at: usize) -> usize {
        while at < self.input.len() && is_blank(self.input[at]) {
            at += 1;
        }
        at
    }

    #[inline]
    fn trim_end(&self, start: usize, mut end: usize) -> usize {
        while end > start && matches!(self.input[end - 1], b' ' | b'\t' | b'\r' | b'\n') {
            end -= 1;
        }
        end
    }

    /// End of an anchor, alias, or tag token.
    #[inline]
    fn token_end(&self, mut at: usize) -> usize {
        while at < self.input.len()
            && !is_blank_or_break(self.input[at])
            && !is_flow_indicator(self.input[at])
        {
            at += 1;
        }
        at
    }

    /// End of input, a blank, or a line break.
    #[inline]
    fn ends_token(&self, at: usize) -> bool {
        self.input
            .get(at)
            .is_none_or(|&byte| is_blank_or_break(byte))
    }

    #[inline]
    fn ends_flow_token(&self, at: usize) -> bool {
        self.input
            .get(at)
            .is_none_or(|&byte| is_blank_or_break(byte) || is_flow_indicator(byte))
    }

    #[inline]
    fn is_entry(&self, at: usize) -> bool {
        self.input[at] == b'-' && self.ends_token(at + 1)
    }

    /// Skip the lines that start before `to`, from the line break at `feed`.
    /// Returns the first line break at or after `to`, or `None` if one of
    /// those lines is a `---` or `...` marker, which no scalar or flow
    /// collection may cross.
    fn breaks_before(&self, mut feed: usize, to: usize) -> Option<usize> {
        while feed < to {
            let line = feed + 1;
            if self.is_document_marker(line, b'-') || self.is_document_marker(line, b'.') {
                return None;
            }
            feed = self.line_end(line);
        }
        Some(feed)
    }

    #[inline]
    fn is_document_marker(&self, line: usize, marker: u8) -> bool {
        self.input
            .get(line..line + 3)
            .is_some_and(|bytes| bytes == [marker; 3])
            && self.ends_token(line + 3)
    }
}

fn merge<'a>(
    outer: YamlProperties<'a>,
    inner: YamlProperties<'a>,
    at: usize,
) -> Result<YamlProperties<'a>, YamlError> {
    if (outer.anchor.is_some() && inner.anchor.is_some())
        || (outer.tag.is_some() && inner.tag.is_some())
    {
        return Err(error(YamlErrorKind::UnexpectedContent, at));
    }
    Ok(YamlProperties {
        anchor: outer.anchor.or(inner.anchor),
        tag: outer.tag.or(inner.tag),
    })
}

#[inline]
fn is_blank(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

#[inline]
fn is_break(byte: u8) -> bool {
    byte == b'\n' || byte == b'\r'
}

#[inline]
fn is_blank_or_break(byte: u8) -> bool {
    is_blank(byte) || is_break(byte)
}

#[inline]
fn is_flow_indicator(byte: u8) -> bool {
    matches!(byte, b',' | b'[' | b']' | b'{' | b'}')
}
