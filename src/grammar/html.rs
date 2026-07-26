use std::fmt;

use crate::grammar::{Grammar, NibbleTables, Utf8Policy, span};
use crate::{CapacityError, ClassifiedBlock, Span};

const PUNCTUATION_BITS: u8 = 1;
const QUOTE_BITS: u8 = 2;
const WHITESPACE_BITS: u8 = 4 | 8;
const SLASH_BITS: u8 = 16;

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

impl Grammar for HtmlGrammar {
    type Config = HtmlConfig;
    type Error = HtmlError;
    type Event<'a> = HtmlEvent<'a>;
    type Stage1State = HtmlStage1State;
    type Stage2State = HtmlStage2State;

    // Bits are separated by high-nibble domains. The low/high intersection is
    // exact for HTML whitespace, quotes, '/', and the '<', '=', '>' group.
    const TABLES: NibbleTables = NibbleTables {
        low: [8, 0, 2, 0, 0, 0, 0, 2, 0, 4, 4, 0, 1, 1 | 4, 1, 16],
        high: [4, 0, 2 | 8 | 16, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    };
    const UTF8_POLICY: Utf8Policy = Utf8Policy::ByteOriented;
    const CLASS_BITS: [u8; 8] = [
        PUNCTUATION_BITS,
        QUOTE_BITS,
        WHITESPACE_BITS,
        SLASH_BITS,
        0,
        0,
        0,
        0,
    ];
    const CLASS_COUNT: usize = 4;
    const EXACT_BYTES: [u8; 4] = [b'<', b'>', b'=', b'"'];
    const EXACT_COUNT: usize = 0;

    #[inline]
    fn scan_block(
        _input: &[u8; 64],
        base: u32,
        valid: u64,
        masks: &ClassifiedBlock,
        _state: &mut Self::Stage1State,
        tape: &mut Vec<u32>,
    ) -> Result<(), Self::Error> {
        let mask = (masks.class[0] | masks.class[1] | masks.class[2] | masks.class[3]) & valid;
        flatten_offsets(mask, base, tape);
        Ok(())
    }

    #[inline]
    fn finish_stage1(_input_len: u32, _state: &mut Self::Stage1State) -> Result<(), Self::Error> {
        Ok(())
    }

    fn parse<'a, F>(
        input: &'a [u8],
        tape: &[u32],
        config: &Self::Config,
        _state: &mut Self::Stage2State,
        mut sink: F,
    ) -> Result<(), Self::Error>
    where
        F: FnMut(Self::Event<'a>),
    {
        let mut tape_index = 0usize;

        while let Some((index, tag_start)) = find_byte(input, tape, tape_index, b'<') {
            tape_index = index + 1;
            let content_start = tag_start + 1;

            if input
                .get(content_start..content_start.saturating_add(3))
                .is_some_and(|prefix| prefix == b"!--")
            {
                tape_index = find_comment_end(input, tape, tape_index).unwrap_or(tape.len());
                continue;
            }

            if matches!(input.get(content_start), Some(b'!' | b'/' | b'?')) {
                tape_index = find_byte(input, tape, tape_index, b'>')
                    .map_or(tape.len(), |(index, _)| index + 1);
                continue;
            }

            let (tag_end, next_tape_index) = find_tag_end(input, tape, tape_index);
            parse_tag(input, content_start, tag_end, config, &mut sink);
            tape_index = next_tape_index;
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

    fn utf8_error(offset: u32) -> Self::Error {
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

#[inline]
fn flatten_offsets(mut mask: u64, base: u32, tape: &mut Vec<u32>) {
    let additional = mask.count_ones() as usize;
    debug_assert!(tape.capacity() - tape.len() >= additional);
    let initial_len = tape.len();
    let mut written = 0usize;
    let output = unsafe { tape.as_mut_ptr().add(initial_len) };

    while mask != 0 {
        let lane = mask.trailing_zeros();
        unsafe { output.add(written).write(base + lane) };
        written += 1;
        mask &= mask - 1;
    }
    unsafe { tape.set_len(initial_len + written) };
}

fn find_byte(input: &[u8], tape: &[u32], mut index: usize, wanted: u8) -> Option<(usize, usize)> {
    while index < tape.len() {
        let offset = tape[index] as usize;
        if input[offset] == wanted {
            return Some((index, offset));
        }
        index += 1;
    }
    None
}

fn find_comment_end(input: &[u8], tape: &[u32], mut index: usize) -> Option<usize> {
    while let Some((found, offset)) = find_byte(input, tape, index, b'>') {
        if offset >= 2 && &input[offset - 2..offset] == b"--" {
            return Some(found + 1);
        }
        index = found + 1;
    }
    None
}

fn find_tag_end(input: &[u8], tape: &[u32], mut index: usize) -> (usize, usize) {
    let mut quote = 0u8;
    while index < tape.len() {
        let offset = tape[index] as usize;
        let byte = input[offset];
        if quote == 0 {
            match byte {
                b'\'' | b'"' => quote = byte,
                b'>' => return (offset, index + 1),
                _ => {}
            }
        } else if byte == quote {
            quote = 0;
        }
        index += 1;
    }
    (input.len(), tape.len())
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
        for byte in u8::MIN..=u8::MAX {
            let class = HtmlGrammar::TABLES.classify(byte);
            assert_eq!(
                class & PUNCTUATION_BITS != 0,
                matches!(byte, b'<' | b'=' | b'>')
            );
            assert_eq!(class & QUOTE_BITS != 0, matches!(byte, b'\'' | b'"'));
            assert_eq!(class & WHITESPACE_BITS != 0, is_html_ws(byte));
            assert_eq!(class & SLASH_BITS != 0, byte == b'/');
        }
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
