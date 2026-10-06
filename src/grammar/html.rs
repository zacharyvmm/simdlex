use std::fmt;

use simdlex_index::{ClassifiedBlock, IndexMode, IndexSpec, IndexView, NibbleTables, Utf8Policy};
use simdlex_tablegen::{ByteSet, Tables, generate, unwrap};

use crate::grammar::{Grammar, span};
use crate::{CapacityError, Span};

/// Stage-1 classes, in mask order: `<`, `>`, `'`, `"`.
///
/// These are the only bytes a tag-level HTML state machine needs to jump
/// between: tag open/close and both quote styles. Whitespace, names, and
/// `=` are read directly from the bytes between structurals. `=` is not
/// indexed: it always follows an attribute name the walker is already
/// reading, so a tape entry for it would only be skipped (it was 13–21% of
/// structurals on real pages, and dropping it sped stage 1 up by 8–12%).
/// Quote regions cannot be masked out in stage 1 the way simdjson masks
/// strings, because a quote only opens a value inside a tag; apostrophes in
/// text content would desynchronise a prefix-XOR quote parity.
const CLASSES: [ByteSet; 4] = [
    ByteSet::from_bytes(b"<"),
    ByteSet::from_bytes(b">"),
    ByteSet::from_bytes(b"'"),
    ByteSet::from_bytes(b"\""),
];

const GENERATED: Tables<4> = unwrap(generate(&CLASSES));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlEmit {
    Candidates,
    Tokens,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlConfig {
    attributes: Vec<Vec<u8>>,
    emit: HtmlEmit,
    matches_class: bool,
    matches_class_name: bool,
}

impl HtmlConfig {
    pub fn new<I, S>(attributes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut attributes: Vec<Vec<u8>> = attributes
            .into_iter()
            .map(|name| name.as_ref().as_bytes().to_vec())
            .collect();
        attributes.sort_by_key(|name| name.len());
        attributes.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        let matches_class = attributes
            .iter()
            .any(|name| name.eq_ignore_ascii_case(b"class"));
        let matches_class_name = attributes
            .iter()
            .any(|name| name.eq_ignore_ascii_case(b"className"));
        Self {
            attributes,
            emit: HtmlEmit::Candidates,
            matches_class,
            matches_class_name,
        }
    }

    #[inline]
    pub fn emit(mut self, emit: HtmlEmit) -> Self {
        self.emit = emit;
        self
    }

    #[inline]
    pub fn emission(&self) -> HtmlEmit {
        self.emit
    }

    pub fn attributes(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.attributes.iter().map(Vec::as_slice)
    }

    #[inline]
    fn matches(&self, candidate: &[u8]) -> bool {
        match candidate.len() {
            5 if self.matches_class && candidate.eq_ignore_ascii_case(b"class") => true,
            9 if self.matches_class_name && candidate.eq_ignore_ascii_case(b"className") => true,
            length => self
                .attributes
                .iter()
                .skip_while(|name| name.len() < length)
                .take_while(|name| name.len() == length)
                .any(|name| candidate.eq_ignore_ascii_case(name)),
        }
    }
}

impl Default for HtmlConfig {
    fn default() -> Self {
        Self::new(["class", "className"])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlEvent<'a> {
    ClassCandidate { raw: &'a [u8], span: Span },
    ClassToken { raw: &'a [u8], span: Span },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HtmlError {
    pub kind: HtmlErrorKind,
    pub offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlErrorKind {
    InputTooLarge,
    Capacity,
}

impl fmt::Display for HtmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} at byte {}", self.kind, self.offset)
    }
}

impl std::error::Error for HtmlError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HtmlStage1State;

#[derive(Debug, Default)]
pub struct HtmlStage2State;

#[derive(Debug, Clone, Copy)]
pub struct HtmlGrammar;

impl IndexSpec for HtmlGrammar {
    type State = HtmlStage1State;
    type Error = HtmlError;

    const TABLES: NibbleTables = NibbleTables {
        low: GENERATED.low,
        high: GENERATED.high,
    };
    const CLASS_BITS: [u8; 8] = GENERATED.padded_class_bits();
    const CLASS_COUNT: usize = CLASSES.len();
    const EXACT_BYTES: [u8; 4] = [0; 4];
    const EXACT_COUNT: usize = 0;
    const UTF8_POLICY: Utf8Policy = Utf8Policy::ByteOriented;
    // Five single-byte delimiters are sparse in markup, so a tape is smaller
    // than per-block masks and lets stage 2 jump directly between them.
    const MODE: IndexMode = IndexMode::Tape;

    #[inline]
    fn scan_block(
        _input: &[u8; 64],
        _base: u32,
        _valid: u64,
        masks: &mut ClassifiedBlock,
        _state: &mut Self::State,
    ) -> Result<u64, Self::Error> {
        Ok(masks.union_of_classes(Self::CLASS_COUNT))
    }

    #[inline]
    fn finish(_input_len: u32, _state: &mut Self::State) -> Result<(), Self::Error> {
        Ok(())
    }

    fn utf8_error(offset: u32) -> Self::Error {
        HtmlError {
            kind: HtmlErrorKind::Capacity,
            offset,
        }
    }
}

impl Grammar for HtmlGrammar {
    type Config = HtmlConfig;
    type Event<'a> = HtmlEvent<'a>;
    type Stage2State = HtmlStage2State;

    fn parse<'a, F>(
        input: &'a [u8],
        index: IndexView<'_>,
        config: &Self::Config,
        _state: &mut Self::Stage2State,
        mut sink: F,
    ) -> Result<(), Self::Error>
    where
        F: FnMut(Self::Event<'a>),
    {
        // Monomorphize the walk per representation rather than dispatching
        // on the mode for every structural.
        match index {
            IndexView::Tape(tape) => parse_tags(input, tape.iter().copied(), config, &mut sink),
            IndexView::Masks(masks) => {
                parse_tags(input, masks.structural_positions(), config, &mut sink)
            }
        }
        Ok(())
    }

    fn input_too_large_error() -> Self::Error {
        HtmlError {
            kind: HtmlErrorKind::InputTooLarge,
            offset: u32::MAX,
        }
    }

    fn capacity_error(offset: u32) -> Self::Error {
        HtmlError {
            kind: HtmlErrorKind::Capacity,
            offset,
        }
    }

    fn reserve_stage2(
        _config: &Self::Config,
        _state: &mut Self::Stage2State,
        _fallible: bool,
    ) -> Result<(), CapacityError> {
        Ok(())
    }
}

fn parse_tags<'a, F>(
    input: &'a [u8],
    mut positions: impl Iterator<Item = u32>,
    config: &HtmlConfig,
    sink: &mut F,
) where
    F: FnMut(HtmlEvent<'a>),
{
    while let Some(tag_start) = find_byte(input, &mut positions, b'<') {
        let content_start = tag_start + 1;

        if input
            .get(content_start..content_start.saturating_add(3))
            .is_some_and(|prefix| prefix == b"!--")
        {
            skip_comment(input, &mut positions);
            continue;
        }

        if matches!(input.get(content_start), Some(b'!' | b'/' | b'?')) {
            find_byte(input, &mut positions, b'>');
            continue;
        }

        let tag_end = find_tag_end(input, &mut positions);
        parse_tag(input, content_start, tag_end, config, sink);
    }
}

#[inline]
fn find_byte(input: &[u8], positions: &mut impl Iterator<Item = u32>, wanted: u8) -> Option<usize> {
    positions
        .map(|offset| offset as usize)
        .find(|&offset| input[offset] == wanted)
}

fn skip_comment(input: &[u8], positions: &mut impl Iterator<Item = u32>) {
    while let Some(offset) = find_byte(input, positions, b'>') {
        if offset >= 2 && &input[offset - 2..offset] == b"--" {
            return;
        }
    }
}

fn find_tag_end(input: &[u8], positions: &mut impl Iterator<Item = u32>) -> usize {
    let mut quote = 0u8;
    for offset in positions {
        let offset = offset as usize;
        let byte = input[offset];
        if quote == 0 {
            match byte {
                b'\'' | b'"' => quote = byte,
                b'>' => return offset,
                _ => {}
            }
        } else if byte == quote {
            quote = 0;
        }
    }
    input.len()
}

fn parse_tag<'a, F>(
    input: &'a [u8],
    mut cursor: usize,
    end: usize,
    config: &HtmlConfig,
    sink: &mut F,
) where
    F: FnMut(HtmlEvent<'a>),
{
    cursor = skip_name(input, cursor, end);

    while cursor < end {
        cursor = skip_ws(input, cursor, end);
        if cursor >= end {
            break;
        }
        if input[cursor] == b'/' {
            cursor += 1;
            continue;
        }

        let name_start = cursor;
        let name_end = skip_attr_name(input, cursor, end);
        if name_start == name_end {
            cursor += 1;
            continue;
        }
        cursor = skip_ws(input, name_end, end);
        if cursor >= end || input[cursor] != b'=' {
            continue;
        }

        cursor += 1;
        cursor = skip_ws(input, cursor, end);
        if cursor >= end {
            break;
        }

        let is_configured = config.matches(&input[name_start..name_end]);
        let (value_start, value_end, next) = parse_attr_value(input, cursor, end);
        if is_configured {
            emit_value(input, value_start, value_end, config.emit, sink);
        }
        cursor = next;
    }
}

fn emit_value<'a, F>(input: &'a [u8], start: usize, end: usize, emit: HtmlEmit, sink: &mut F)
where
    F: FnMut(HtmlEvent<'a>),
{
    if emit == HtmlEmit::Candidates {
        sink(HtmlEvent::ClassCandidate {
            raw: &input[start..end],
            span: span(start, end),
        });
        return;
    }

    let mut cursor = start;
    while cursor < end {
        while cursor < end && is_html_ws(input[cursor]) {
            cursor += 1;
        }
        let token_start = cursor;
        while cursor < end && !is_html_ws(input[cursor]) {
            cursor += 1;
        }
        if token_start != cursor {
            sink(HtmlEvent::ClassToken {
                raw: &input[token_start..cursor],
                span: span(token_start, cursor),
            });
        }
    }
}

fn parse_attr_value(input: &[u8], cursor: usize, end: usize) -> (usize, usize, usize) {
    if matches!(input[cursor], b'\'' | b'"') {
        let quote = input[cursor];
        let start = cursor + 1;
        let close = input[start..end]
            .iter()
            .position(|&byte| byte == quote)
            .map_or(end, |relative| start + relative);
        (start, close, close.saturating_add(1).min(end))
    } else {
        let start = cursor;
        let value_end = input[start..end]
            .iter()
            .position(|&byte| is_html_ws(byte) || byte == b'>')
            .map_or(end, |relative| start + relative);
        (start, value_end, value_end)
    }
}

fn skip_name(input: &[u8], mut cursor: usize, end: usize) -> usize {
    while cursor < end && !is_html_ws(input[cursor]) && !matches!(input[cursor], b'/' | b'>') {
        cursor += 1;
    }
    cursor
}

fn skip_attr_name(input: &[u8], mut cursor: usize, end: usize) -> usize {
    while cursor < end && !is_html_ws(input[cursor]) && !matches!(input[cursor], b'=' | b'/' | b'>')
    {
        cursor += 1;
    }
    cursor
}

fn skip_ws(input: &[u8], mut cursor: usize, end: usize) -> usize {
    while cursor < end && is_html_ws(input[cursor]) {
        cursor += 1;
    }
    cursor
}

#[inline]
fn is_html_ws(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | b'\r' | b' ')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BackendKind, Parser};

    #[test]
    fn table_is_exact_for_named_sets() {
        let delimiters = b"<>'\"";
        for byte in u8::MIN..=u8::MAX {
            let class = HtmlGrammar::TABLES.classify(byte);
            for (selected, &delimiter) in delimiters.iter().enumerate() {
                let bits = HtmlGrammar::CLASS_BITS[selected];
                assert_eq!(class & bits != 0, byte == delimiter);
            }
        }
    }

    #[test]
    fn tape_holds_only_tag_delimiters() {
        let input = b"<p a = 'x' b=\"y\"> it's / done</p>";
        let mut parser = Parser::<HtmlGrammar>::new(HtmlConfig::default());
        parser.stage1(input).unwrap();
        let tape = parser.index().tape().unwrap();
        let bytes: Vec<u8> = tape.iter().map(|&offset| input[offset as usize]).collect();
        assert_eq!(bytes, b"<''\"\">'<>");
    }

    #[test]
    fn parser_honors_config_and_tape_boundaries() {
        let input = br#"<!-- <x data-x="no"> --><DIV className='a b' title=">"><x data-x=z>"#;
        let config = HtmlConfig::new(["className", "data-x"]);
        let mut parser = Parser::<HtmlGrammar>::with_backend(config, BackendKind::Scalar).unwrap();
        let mut values = Vec::new();
        parser
            .parse_with(input, |event| {
                if let HtmlEvent::ClassCandidate { raw, .. } = event {
                    values.push(raw);
                }
            })
            .unwrap();
        assert_eq!(values, [b"a b".as_slice(), b"z".as_slice()]);
    }
}
