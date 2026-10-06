//! A StAX-style HTML element stream: stage 2 over the delimiter index.
//!
//! [`HtmlReader`] is a pull parser. It walks the structural offsets produced
//! by stage 1 (`<`, `>`, `=`, `'`, `"`) and yields flat events. It builds no
//! tree, so it does not balance or imply tags. Text between delimiters is
//! never examined byte by byte: the reader jumps from one `<` to the next.
//! Inside a tag it reads names and whitespace directly, and it uses the index
//! to jump to the closing quote of a quoted attribute value.
//!
//! Tokenization follows the WHATWG HTML tokenizer for the data, tag,
//! attribute, comment, DOCTYPE, and bogus-comment states, with these
//! simplifications:
//!
//! - character references (`&amp;`) are not decoded;
//! - DOCTYPE contents are not split into name and identifiers;
//! - the contents of raw-text elements (`script`, `style`, `textarea`,
//!   `title`, `xmp`, `iframe`, `noembed`, `noframes`) end at the first
//!   matching end tag, without the script-data escape states. `plaintext`
//!   runs to the end of input;
//! - a tag cut off by the end of input is dropped, as the specification
//!   requires, and ends the stream.
//!
//! ```
//! use simdlex::{HtmlReader, HtmlStreamEvent, HtmlStreamGrammar, Parser};
//!
//! let input = br#"<p class="intro">Hi<br/></p>"#;
//! let mut parser = Parser::<HtmlStreamGrammar>::new(());
//! parser.stage1(input)?;
//!
//! for event in HtmlReader::new(input, parser.index()) {
//!     if let HtmlStreamEvent::StartElement(element) = event {
//!         for attribute in element.attributes() {
//!             println!("{:?}={:?}", attribute.name, attribute.value.map(|v| v.raw));
//!         }
//!     }
//! }
//! # Ok::<(), simdlex::HtmlError>(())
//! ```

use std::fmt;

use simdlex_index::{ClassifiedBlock, IndexMode, IndexSpec, IndexView, NibbleTables, Positions};

use crate::grammar::{Grammar, span};
use crate::{CapacityError, HtmlError, HtmlGrammar, HtmlStage1State, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlStreamEvent<'a> {
    StartElement(HtmlStartElement<'a>),
    /// `</name ...>`. HtmlAttributes on end tags are parsed and discarded.
    EndElement {
        name: &'a [u8],
        span: Span,
    },
    /// Character data, with references undecoded. Raw-text element contents
    /// are reported as a single `Text`.
    Text {
        raw: &'a [u8],
        span: Span,
    },
    /// The contents of `<!-- ... -->`.
    Comment {
        raw: &'a [u8],
        span: Span,
    },
    /// Everything after `<!DOCTYPE` up to `>`, without leading whitespace.
    Doctype {
        raw: &'a [u8],
        span: Span,
    },
    /// `<?...>`, `<!...>`, and `</ ...>` constructs, which HTML treats as
    /// comments.
    BogusComment {
        raw: &'a [u8],
        span: Span,
    },
}

/// A start tag. HtmlAttributes are parsed lazily from the tag's bytes.
#[derive(Clone, Copy)]
pub struct HtmlStartElement<'a> {
    pub name: &'a [u8],
    /// The whole tag, from `<` through `>`.
    pub span: Span,
    /// Whether the tag ended with `/>`. HTML ignores this flag except on
    /// foreign elements; it is reported as written.
    pub self_closing: bool,
    input: &'a [u8],
    attributes_start: u32,
    attributes_end: u32,
}

impl<'a> HtmlStartElement<'a> {
    #[inline]
    pub fn name_eq_ignore_ascii_case(&self, name: &[u8]) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    #[inline]
    pub fn attributes(&self) -> HtmlAttributes<'a> {
        HtmlAttributes {
            input: &self.input[..self.attributes_end as usize],
            cursor: self.attributes_start as usize,
        }
    }

    /// The first attribute whose name matches `name`, ignoring ASCII case.
    /// Later duplicates are ignored, as in HTML.
    pub fn attribute(&self, name: &[u8]) -> Option<HtmlAttribute<'a>> {
        self.attributes()
            .find(|attribute| attribute.name.eq_ignore_ascii_case(name))
    }
}

impl PartialEq for HtmlStartElement<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.span == other.span
            && self.self_closing == other.self_closing
            && self.attributes_start == other.attributes_start
            && self.attributes_end == other.attributes_end
    }
}

impl Eq for HtmlStartElement<'_> {}

impl fmt::Debug for HtmlStartElement<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HtmlStartElement")
            .field("name", &String::from_utf8_lossy(self.name))
            .field("span", &self.span)
            .field("self_closing", &self.self_closing)
            .field("attributes", &self.attributes().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HtmlAttribute<'a> {
    pub name: &'a [u8],
    pub name_span: Span,
    /// `None` for a bare attribute such as `disabled`.
    pub value: Option<HtmlAttributeValue<'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HtmlAttributeValue<'a> {
    /// The value without quotes, with references undecoded.
    pub raw: &'a [u8],
    pub span: Span,
    /// `Some(b'"')` or `Some(b'\'')` for quoted values.
    pub quote: Option<u8>,
}

/// HtmlAttributes of one start tag, in source order.
#[derive(Debug, Clone)]
pub struct HtmlAttributes<'a> {
    /// Input truncated before the tag's closing `>` (or `/>`).
    input: &'a [u8],
    cursor: usize,
}

impl<'a> Iterator for HtmlAttributes<'a> {
    type Item = HtmlAttribute<'a>;

    fn next(&mut self) -> Option<HtmlAttribute<'a>> {
        let input = self.input;
        let mut find_quote = |from: usize, quote: u8| {
            input[from..]
                .iter()
                .position(|&byte| byte == quote)
                .map(|relative| from + relative)
        };
        match tag_step(input, &mut self.cursor, &mut find_quote) {
            TagStep::Attribute { name, value } => Some(attribute(input, name, value)),
            TagStep::End { .. } | TagStep::Eof => None,
        }
    }
}

fn attribute(
    input: &[u8],
    (name_start, name_end): (usize, usize),
    value: Option<(usize, usize, Option<u8>)>,
) -> HtmlAttribute<'_> {
    HtmlAttribute {
        name: &input[name_start..name_end],
        name_span: span(name_start, name_end),
        value: value.map(|(start, end, quote)| HtmlAttributeValue {
            raw: &input[start..end],
            span: span(start, end),
            quote,
        }),
    }
}

/// One step of the before-attribute-name through after-attribute-value
/// states.
enum TagStep {
    Attribute {
        name: (usize, usize),
        value: Option<(usize, usize, Option<u8>)>,
    },
    End {
        gt: usize,
        self_closing: bool,
    },
    Eof,
}

/// Advance over one attribute or the end of a tag. `find_quote(from, quote)`
/// returns the first offset at or after `from` holding `quote`. Stage 2
/// answers it from the structural index; [`HtmlAttributes`] scans bytes.
#[inline]
fn tag_step(
    input: &[u8],
    cursor: &mut usize,
    find_quote: &mut impl FnMut(usize, u8) -> Option<usize>,
) -> TagStep {
    let len = input.len();
    let mut at = *cursor;
    loop {
        at = skip_ws(input, at);
        if at >= len {
            *cursor = len;
            return TagStep::Eof;
        }
        match input[at] {
            b'>' => {
                *cursor = at + 1;
                return TagStep::End {
                    gt: at,
                    self_closing: false,
                };
            }
            b'/' if input.get(at + 1) == Some(&b'>') => {
                *cursor = at + 2;
                return TagStep::End {
                    gt: at + 1,
                    self_closing: true,
                };
            }
            b'/' => at += 1,
            _ => break,
        }
    }

    // The first name byte is always part of the name, even if it is `=`.
    let name_start = at;
    at += 1;
    while at < len && !is_ws(input[at]) && !matches!(input[at], b'/' | b'>' | b'=') {
        at += 1;
    }
    let name = (name_start, at);

    let after_name = skip_ws(input, at);
    if after_name >= len || input[after_name] != b'=' {
        *cursor = after_name;
        return TagStep::Attribute { name, value: None };
    }

    at = skip_ws(input, after_name + 1);
    if at >= len {
        // `name=` then end of input. Stage 2 drops the tag on its next step;
        // `HtmlAttributes` reads input truncated before `>`, where this is a
        // missing value.
        *cursor = len;
        return TagStep::Attribute {
            name,
            value: Some((len, len, None)),
        };
    }
    let value = match input[at] {
        quote @ (b'"' | b'\'') => {
            let Some(close) = find_quote(at + 1, quote) else {
                *cursor = len;
                return TagStep::Eof;
            };
            *cursor = close + 1;
            (at + 1, close, Some(quote))
        }
        // A missing value is empty; the `>` still closes the tag.
        b'>' => {
            *cursor = at;
            (at, at, None)
        }
        _ => {
            let start = at;
            while at < len && !is_ws(input[at]) && input[at] != b'>' {
                at += 1;
            }
            *cursor = at;
            (start, at, None)
        }
    };
    TagStep::Attribute {
        name,
        value: Some(value),
    }
}

/// Forward-only search over structural offsets.
///
/// The most recent match stays peeked, so searching again from an offset at
/// or before it returns it again. Offsets are only discarded once a search
/// starts past them or they hold a different byte; callers search from
/// non-decreasing offsets, so nothing they need is lost.
struct Structurals<P> {
    positions: P,
    peeked: Option<usize>,
}

impl<P: Iterator<Item = u32>> Structurals<P> {
    #[inline]
    fn find(&mut self, input: &[u8], from: usize, wanted: u8) -> Option<usize> {
        loop {
            let offset = match self.peeked.take() {
                Some(offset) => offset,
                None => self.positions.next()? as usize,
            };
            if offset >= from && input[offset] == wanted {
                self.peeked = Some(offset);
                return Some(offset);
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Content {
    /// Ordinary markup.
    Data,
    /// Inside a raw-text element whose name is `input[start..end]`.
    RawText { start: usize, end: usize },
    /// Inside `plaintext`: everything left is text.
    Plaintext,
}

/// A pull parser yielding [`HtmlStreamEvent`]s.
///
/// `positions` must yield, in ascending order, at least every offset of `<`,
/// `>`, `'`, and `"` in `input`. Extra offsets are skipped, so
/// `0..input.len() as u32` is a valid (slow) reference index.
pub struct HtmlReader<'a, P> {
    input: &'a [u8],
    structurals: Structurals<P>,
    position: usize,
    content: Content,
}

impl<'a, 'i> HtmlReader<'a, Positions<'i>> {
    /// Read `input` using the index built from it by stage 1, in either mode.
    pub fn new(input: &'a [u8], index: IndexView<'i>) -> Self {
        Self::from_positions(input, index.positions())
    }
}

impl<'a, P: Iterator<Item = u32>> HtmlReader<'a, P> {
    pub fn from_positions(input: &'a [u8], positions: P) -> Self {
        Self {
            input,
            structurals: Structurals {
                positions,
                peeked: None,
            },
            position: 0,
            content: Content::Data,
        }
    }

    fn text(&mut self, end: usize) -> Option<HtmlStreamEvent<'a>> {
        let start = self.position;
        self.position = end;
        (start < end).then(|| HtmlStreamEvent::Text {
            raw: &self.input[start..end],
            span: span(start, end),
        })
    }

    /// The next `<` that starts markup rather than text.
    fn next_markup(&mut self, mut from: usize) -> Option<usize> {
        let input = self.input;
        loop {
            let lt = self.structurals.find(input, from, b'<')?;
            match input.get(lt + 1) {
                Some(byte) if byte.is_ascii_alphabetic() || matches!(byte, b'!' | b'?') => {
                    return Some(lt);
                }
                Some(b'/') if lt + 2 < input.len() => return Some(lt),
                _ => from = lt + 1,
            }
        }
    }

    /// The `<` of the end tag closing raw-text element `name`.
    fn raw_text_end(&mut self, name: &[u8]) -> Option<usize> {
        let input = self.input;
        let mut from = self.position;
        loop {
            let lt = self.structurals.find(input, from, b'<')?;
            from = lt + 1;
            let name_start = lt + 2;
            let name_end = name_start + name.len();
            if input.get(lt + 1) == Some(&b'/')
                && input
                    .get(name_start..name_end)
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
                && input
                    .get(name_end)
                    .is_some_and(|&byte| is_ws(byte) || matches!(byte, b'/' | b'>'))
            {
                return Some(lt);
            }
        }
    }

    /// Tokenize the markup at `lt`. Returns `None` when it produces no
    /// event (`</>`, or a tag cut off by the end of input).
    fn markup(&mut self, lt: usize) -> Option<HtmlStreamEvent<'a>> {
        let input = self.input;
        match input[lt + 1] {
            b'/' if input[lt + 2].is_ascii_alphabetic() => self.end_tag(lt),
            b'/' if input[lt + 2] == b'>' => {
                self.position = lt + 3;
                None
            }
            b'/' => Some(self.bogus_comment(lt, lt + 2)),
            b'!' if input[lt + 2..].starts_with(b"--") => Some(self.comment(lt)),
            b'!' if input
                .get(lt + 2..lt + 9)
                .is_some_and(|keyword| keyword.eq_ignore_ascii_case(b"DOCTYPE")) =>
            {
                Some(self.doctype(lt))
            }
            b'!' => Some(self.bogus_comment(lt, lt + 2)),
            b'?' => Some(self.bogus_comment(lt, lt + 1)),
            _ => self.start_tag(lt),
        }
    }

    fn tag_name(&self, start: usize) -> usize {
        let input = self.input;
        let mut end = start;
        while end < input.len() && !is_ws(input[end]) && !matches!(input[end], b'/' | b'>') {
            end += 1;
        }
        end
    }

    /// Step over a tag's attributes. Returns `(gt, self_closing)`, or `None`
    /// at the end of input.
    fn finish_tag(&mut self, mut cursor: usize) -> Option<(usize, bool)> {
        let input = self.input;
        let structurals = &mut self.structurals;
        let mut find_quote = |from: usize, quote: u8| structurals.find(input, from, quote);
        loop {
            match tag_step(input, &mut cursor, &mut find_quote) {
                TagStep::Attribute { .. } => {}
                TagStep::End { gt, self_closing } => return Some((gt, self_closing)),
                TagStep::Eof => return None,
            }
        }
    }

    fn start_tag(&mut self, lt: usize) -> Option<HtmlStreamEvent<'a>> {
        let name_end = self.tag_name(lt + 1);
        let Some((gt, self_closing)) = self.finish_tag(name_end) else {
            self.position = self.input.len();
            return None;
        };
        self.position = gt + 1;

        let name = &self.input[lt + 1..name_end];
        if RAW_TEXT_ELEMENTS
            .iter()
            .any(|element| name.eq_ignore_ascii_case(element))
        {
            self.content = Content::RawText {
                start: lt + 1,
                end: name_end,
            };
        } else if name.eq_ignore_ascii_case(b"plaintext") {
            self.content = Content::Plaintext;
        }

        Some(HtmlStreamEvent::StartElement(HtmlStartElement {
            name,
            span: span(lt, gt + 1),
            self_closing,
            input: self.input,
            attributes_start: name_end as u32,
            attributes_end: (if self_closing { gt - 1 } else { gt }) as u32,
        }))
    }

    fn end_tag(&mut self, lt: usize) -> Option<HtmlStreamEvent<'a>> {
        let name_end = self.tag_name(lt + 2);
        let Some((gt, _)) = self.finish_tag(name_end) else {
            self.position = self.input.len();
            return None;
        };
        self.position = gt + 1;
        Some(HtmlStreamEvent::EndElement {
            name: &self.input[lt + 2..name_end],
            span: span(lt, gt + 1),
        })
    }

    fn comment(&mut self, lt: usize) -> HtmlStreamEvent<'a> {
        let input = self.input;
        let content_start = lt + 4;
        let mut from = content_start;
        let (content_end, end) = loop {
            let Some(gt) = self.structurals.find(input, from, b'>') else {
                break (input.len(), input.len());
            };
            from = gt + 1;
            // `-->` may overlap the opener (`<!-->`, `<!--->`); `--!>` may not.
            if input[gt - 2..gt] == *b"--" {
                break ((gt - 2).max(content_start), gt + 1);
            }
            if gt >= content_start + 3 && input[gt - 3..gt] == *b"--!" {
                break (gt - 3, gt + 1);
            }
        };
        self.position = end;
        HtmlStreamEvent::Comment {
            raw: &input[content_start..content_end],
            span: span(content_start, content_end),
        }
    }

    /// Content from `start` up to the next `>` (or the end of input).
    fn until_gt(&mut self, start: usize) -> (usize, usize) {
        let input = self.input;
        match self.structurals.find(input, start, b'>') {
            Some(gt) => {
                self.position = gt + 1;
                (start, gt)
            }
            None => {
                self.position = input.len();
                (start, input.len())
            }
        }
    }

    fn doctype(&mut self, lt: usize) -> HtmlStreamEvent<'a> {
        let (start, end) = self.until_gt(lt + 9);
        let start = skip_ws(self.input, start).min(end);
        HtmlStreamEvent::Doctype {
            raw: &self.input[start..end],
            span: span(start, end),
        }
    }

    fn bogus_comment(&mut self, _lt: usize, start: usize) -> HtmlStreamEvent<'a> {
        let (start, end) = self.until_gt(start);
        HtmlStreamEvent::BogusComment {
            raw: &self.input[start..end],
            span: span(start, end),
        }
    }
}

impl<'a, P: Iterator<Item = u32>> Iterator for HtmlReader<'a, P> {
    type Item = HtmlStreamEvent<'a>;

    fn next(&mut self) -> Option<HtmlStreamEvent<'a>> {
        let len = self.input.len();
        loop {
            if self.position >= len {
                return None;
            }

            match self.content {
                Content::Data => {}
                Content::RawText { start, end } => {
                    self.content = Content::Data;
                    let name = &self.input[start..end];
                    let close = self.raw_text_end(name).unwrap_or(len);
                    if let Some(text) = self.text(close) {
                        return Some(text);
                    }
                    continue;
                }
                Content::Plaintext => return self.text(len),
            }

            let Some(lt) = self.next_markup(self.position) else {
                return self.text(len);
            };
            if lt > self.position {
                return self.text(lt);
            }
            if let Some(event) = self.markup(lt) {
                return Some(event);
            }
        }
    }
}

const RAW_TEXT_ELEMENTS: [&[u8]; 8] = [
    b"script",
    b"style",
    b"textarea",
    b"title",
    b"xmp",
    b"iframe",
    b"noembed",
    b"noframes",
];

#[inline]
fn is_ws(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

#[inline]
fn skip_ws(input: &[u8], mut at: usize) -> usize {
    while at < input.len() && is_ws(input[at]) {
        at += 1;
    }
    at
}

/// The HTML stage-1 index with a StAX event stream as stage 2.
///
/// Use it through [`crate::Parser`] for push-style events, or run
/// [`crate::Parser::stage1`] and pull from [`HtmlReader::new`].
#[derive(Debug, Clone, Copy)]
pub struct HtmlStreamGrammar;

impl IndexSpec for HtmlStreamGrammar {
    type State = HtmlStage1State;
    type Error = HtmlError;

    const TABLES: NibbleTables = HtmlGrammar::TABLES;
    const CLASS_BITS: [u8; 8] = HtmlGrammar::CLASS_BITS;
    const CLASS_COUNT: usize = HtmlGrammar::CLASS_COUNT;
    const EXACT_BYTES: [u8; 4] = HtmlGrammar::EXACT_BYTES;
    const EXACT_COUNT: usize = HtmlGrammar::EXACT_COUNT;
    const UTF8_POLICY: simdlex_index::Utf8Policy = HtmlGrammar::UTF8_POLICY;
    const MODE: IndexMode = HtmlGrammar::MODE;

    #[inline]
    fn scan_block(
        input: &[u8; 64],
        base: u32,
        valid: u64,
        masks: &mut ClassifiedBlock,
        state: &mut Self::State,
    ) -> Result<u64, Self::Error> {
        HtmlGrammar::scan_block(input, base, valid, masks, state)
    }

    #[inline]
    fn finish(input_len: u32, state: &mut Self::State) -> Result<(), Self::Error> {
        HtmlGrammar::finish(input_len, state)
    }

    fn utf8_error(offset: u32) -> Self::Error {
        HtmlGrammar::utf8_error(offset)
    }
}

impl Grammar for HtmlStreamGrammar {
    type Config = ();
    type Event<'a> = HtmlStreamEvent<'a>;
    type Stage2State = ();

    fn parse<'a, F>(
        input: &'a [u8],
        index: IndexView<'_>,
        _config: &Self::Config,
        _state: &mut Self::Stage2State,
        sink: F,
    ) -> Result<(), Self::Error>
    where
        F: FnMut(Self::Event<'a>),
    {
        // Monomorphize the reader per representation.
        match index {
            IndexView::Tape(tape) => {
                HtmlReader::from_positions(input, tape.iter().copied()).for_each(sink)
            }
            IndexView::Masks(masks) => {
                HtmlReader::from_positions(input, masks.structural_positions()).for_each(sink)
            }
        }
        Ok(())
    }

    fn input_too_large_error() -> Self::Error {
        HtmlGrammar::input_too_large_error()
    }

    fn capacity_error(offset: u32) -> Self::Error {
        HtmlGrammar::capacity_error(offset)
    }

    fn reserve_stage2(
        _config: &Self::Config,
        _state: &mut Self::Stage2State,
        _fallible: bool,
    ) -> Result<(), CapacityError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BackendKind, HtmlConfig, HtmlEvent, Parser};

    /// A compact rendering of an event stream.
    fn render<'a>(events: impl Iterator<Item = HtmlStreamEvent<'a>>) -> Vec<String> {
        let text = |raw: &[u8]| String::from_utf8_lossy(raw).into_owned();
        events
            .map(|event| match event {
                HtmlStreamEvent::StartElement(element) => {
                    let mut out = format!("+{}", text(element.name));
                    for attribute in element.attributes() {
                        out.push(' ');
                        out.push_str(&text(attribute.name));
                        if let Some(value) = attribute.value {
                            let quote = value.quote.map_or(String::new(), |q| (q as char).into());
                            out.push_str(&format!("={quote}{}{quote}", text(value.raw)));
                        }
                    }
                    if element.self_closing {
                        out.push('/');
                    }
                    out
                }
                HtmlStreamEvent::EndElement { name, .. } => format!("-{}", text(name)),
                HtmlStreamEvent::Text { raw, .. } => format!("|{}", text(raw)),
                HtmlStreamEvent::Comment { raw, .. } => format!("!{}", text(raw)),
                HtmlStreamEvent::Doctype { raw, .. } => format!("D{}", text(raw)),
                HtmlStreamEvent::BogusComment { raw, .. } => format!("?{}", text(raw)),
            })
            .collect()
    }

    /// Events from the real stage-1 index, checked against a reference
    /// "index" that lists every offset.
    fn stream(input: &[u8]) -> Vec<String> {
        let reference = render(HtmlReader::from_positions(input, 0..input.len() as u32));
        let mut parser = Parser::<HtmlStreamGrammar>::new(());
        parser.stage1(input).unwrap();
        assert_eq!(render(HtmlReader::new(input, parser.index())), reference);
        reference
    }

    #[test]
    fn streams_a_document() {
        let input = b"<!DOCTYPE html><html lang=en><body class=\"a b\">\
            <p>Hi &amp; bye<br/></p><!-- note --></body></html>";
        assert_eq!(
            stream(input),
            [
                "Dhtml",
                "+html lang=en",
                "+body class=\"a b\"",
                "+p",
                "|Hi &amp; bye",
                "+br/",
                "-p",
                "! note ",
                "-body",
                "-html",
            ]
        );
    }

    #[test]
    fn tokenizes_attributes_like_html() {
        let input = br#"<a href=/x/ title="a>b" data-q='"' disabled c = d =e f=>"#;
        assert_eq!(
            stream(input),
            [r#"+a href=/x/ title="a>b" data-q='"' disabled c=d =e f="#]
        );

        let input = br#"<img src="x" /><br/><p class="A" CLASS="B">"#;
        assert_eq!(
            stream(input),
            [r#"+img src="x"/"#, "+br/", r#"+p class="A" CLASS="B""#]
        );

        let mut reader = HtmlReader::from_positions(input, 0..input.len() as u32);
        let Some(HtmlStreamEvent::StartElement(img)) = reader.next() else {
            panic!("expected img");
        };
        let value = img.attribute(b"SRC").unwrap().value.unwrap();
        assert_eq!((value.raw, value.span), (b"x".as_slice(), span(10, 11)));
        let Some(HtmlStreamEvent::StartElement(p)) = reader.nth(1) else {
            panic!("expected p");
        };
        assert_eq!(p.attribute(b"class").unwrap().value.unwrap().raw, b"A");
    }

    #[test]
    fn treats_stray_markup_as_text_or_bogus_comments() {
        assert_eq!(stream(b"a < b <3 <>"), ["|a < b <3 <>"]);
        assert_eq!(stream(b"a</>b"), ["|a", "|b"]);
        assert_eq!(stream(b"x</ y>z"), ["|x", "? y", "|z"]);
        assert_eq!(stream(b"x</"), ["|x</"]);
        assert_eq!(
            stream(br#"<?xml version="1.0"?><![CDATA[x]]><!x>"#),
            [r#"??xml version="1.0"?"#, "?[CDATA[x]]", "?x"]
        );
        assert_eq!(stream(b"</p class=\"x>\">t"), ["-p", "|t"]);
    }

    #[test]
    fn finds_comment_ends() {
        assert_eq!(
            stream(b"<!----><!--><!---><!-- a -- b --!><!--a>b-->"),
            ["!", "!", "!", "! a -- b ", "!a>b"]
        );
        assert_eq!(stream(b"<!--x>"), ["!x>"]);
        assert_eq!(stream(b"<!--!>-->"), ["!!>"]);
    }

    #[test]
    fn skips_markup_inside_raw_text_elements() {
        let input = br#"<script>if (a<b && c>d) { s = "</div>"; }</script><style>p>a{}</STYLE ><title>A &lt; <b></title>"#;
        assert_eq!(
            stream(input),
            [
                "+script",
                r#"|if (a<b && c>d) { s = "</div>"; }"#,
                "-script",
                "+style",
                "|p>a{}",
                "-STYLE",
                "+title",
                "|A &lt; <b>",
                "-title",
            ]
        );
        assert_eq!(
            stream(b"<script>x</scripty></script>"),
            ["+script", "|x</scripty>", "-script"]
        );
        assert_eq!(
            stream(b"<textarea>never closed"),
            ["+textarea", "|never closed"]
        );
        assert_eq!(stream(b"<plaintext><a></a>"), ["+plaintext", "|<a></a>"]);
    }

    #[test]
    fn drops_tags_cut_off_by_end_of_input() {
        assert_eq!(stream(br#"<p>x<a href="y"#), ["+p", "|x"]);
        assert_eq!(stream(b"<p>x</p"), ["+p", "|x"]);
        assert_eq!(stream(b"<p a="), Vec::<String>::new());
        assert_eq!(stream(b"<!DOCTYPE html"), ["Dhtml"]);
    }

    #[test]
    fn every_backend_and_mode_matches_the_reference() {
        let template: &[u8] = br#"<!doctype html><div title="> q" class='a b'>it's <x-y/>
            <script>a<b</script><!-- > --><p data-x=1>t</p></div>"#;

        for prefix in 0..16 {
            for tail in [0, 1, 7, 63, 64, 65] {
                let mut storage = vec![b'x'; prefix];
                storage.extend_from_slice(template);
                storage.extend(std::iter::repeat_n(b' ', tail));
                let input = &storage[prefix..];
                let expected = render(HtmlReader::from_positions(input, 0..input.len() as u32));

                for backend in [BackendKind::Scalar, BackendKind::Ssse3, BackendKind::Avx2] {
                    if simdlex_index::resolve_backend(backend).is_err() {
                        continue;
                    }
                    for mode in [IndexMode::Tape, IndexMode::Masks] {
                        let mut parser = Parser::<HtmlStreamGrammar>::with_backend((), backend)
                            .unwrap()
                            .with_index_mode(mode)
                            .unwrap();
                        let mut pushed = Vec::new();
                        parser
                            .parse_with(input, |event| pushed.push(event))
                            .unwrap();
                        assert_eq!(render(pushed.into_iter()), expected);
                        assert_eq!(render(HtmlReader::new(input, parser.index())), expected);
                    }
                }
            }
        }
    }

    #[test]
    fn random_markup_matches_the_reference_in_both_modes() {
        const ALPHABET: &[u8] = b"<<>>/!-='\" ab?\nsS";
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut tape = Parser::<HtmlStreamGrammar>::new(());
        let mut masks = Parser::<HtmlStreamGrammar>::new(())
            .with_index_mode(IndexMode::Masks)
            .unwrap();

        for _ in 0..2_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let len = (seed % 150) as usize;
            let input: Vec<u8> = (0..len)
                .map(|i| ALPHABET[((seed >> (i % 58)) as usize + i * 7) % ALPHABET.len()])
                .collect();
            let input = input.as_slice();
            let expected: Vec<_> =
                HtmlReader::from_positions(input, 0..input.len() as u32).collect();
            for parser in [&mut tape, &mut masks] {
                parser.stage1(input).unwrap();
                let actual: Vec<_> = HtmlReader::new(input, parser.index()).collect();
                assert_eq!(actual, expected, "{:?}", String::from_utf8_lossy(input));
            }
        }
    }

    #[test]
    fn class_attributes_match_the_class_extractor() {
        let documents: &[&[u8]] = &[
            br#"<div class="px-4 font-bold"><span class='a'></span><b class=c>x</b></div>"#,
            br#"<!-- <p class="no"> --><p CLASS="yes" title=">"><i class="">"#,
            br#"<a className="x y" class = "z"><br class=q/>"#,
        ];
        for &input in documents {
            let mut extractor = Parser::<HtmlGrammar>::new(HtmlConfig::new(["class"]));
            let mut expected = Vec::new();
            extractor
                .parse_with(input, |event| {
                    if let HtmlEvent::ClassCandidate { span, .. } = event {
                        expected.push(span);
                    }
                })
                .unwrap();

            let actual: Vec<Span> = HtmlReader::from_positions(input, 0..input.len() as u32)
                .filter_map(|event| match event {
                    HtmlStreamEvent::StartElement(element) => element.attribute(b"class"),
                    _ => None,
                })
                .filter_map(|attribute| attribute.value.map(|value| value.span))
                .collect();
            assert_eq!(actual, expected);
        }
    }
}
