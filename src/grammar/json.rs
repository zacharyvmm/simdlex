use std::fmt;

use crate::grammar::{Grammar, NibbleTables, Utf8Policy, span};
use crate::{CapacityError, ClassifiedBlock, Span};

pub const OPERATOR_BITS: u8 = 1 | 2 | 4;
pub const JSON_WHITESPACE_BITS: u8 = 8 | 16;
pub const QUOTE_BITS: u8 = 32;
pub const BACKSLASH_BITS: u8 = 64;
pub const CONTROL_BITS: u8 = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonConfig {
    pub max_depth: usize,
}

impl Default for JsonConfig {
    fn default() -> Self {
        Self { max_depth: 1_024 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawJsonString<'a> {
    pub raw: &'a [u8],
    pub span: Span,
}

impl<'a> RawJsonString<'a> {
    #[inline]
    pub fn as_bytes(self) -> &'a [u8] {
        self.raw
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonEvent<'a> {
    StartObject { offset: u32 },
    ObjectKey { raw: RawJsonString<'a> },
    EndObject { offset: u32 },
    StartArray { offset: u32 },
    EndArray { offset: u32 },
    String { raw: RawJsonString<'a> },
    Number { raw: &'a [u8], span: Span },
    Boolean { value: bool, span: Span },
    Null { span: Span },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonError {
    pub kind: JsonErrorKind,
    pub offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonErrorKind {
    Utf8,
    InvalidEscape,
    InvalidNumber,
    InvalidLiteral,
    InvalidSurrogate,
    ControlCharacter,
    Structure,
    Depth,
    Capacity,
    InputTooLarge,
    TrailingData,
    UnexpectedEof,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} at byte {}", self.kind, self.offset)
    }
}

impl std::error::Error for JsonError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonStage1State {
    in_string: bool,
    escaped: bool,
    previous_is_delimiter: bool,
}

impl Default for JsonStage1State {
    fn default() -> Self {
        Self {
            in_string: false,
            escaped: false,
            previous_is_delimiter: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Container {
    Object,
    Array,
}

#[derive(Debug, Default)]
pub struct JsonStage2State {
    frames: Vec<Container>,
}

#[derive(Debug, Clone, Copy)]
pub struct JsonGrammar;

impl Grammar for JsonGrammar {
    type Config = JsonConfig;
    type Error = JsonError;
    type Event<'a> = JsonEvent<'a>;
    type Stage1State = JsonStage1State;
    type Stage2State = JsonStage2State;

    const TABLES: NibbleTables = NibbleTables {
        low: [
            16 | 128,
            128,
            32 | 128,
            128,
            128,
            128,
            128,
            128,
            128,
            8 | 128,
            2 | 8 | 128,
            4 | 128,
            1 | 64 | 128,
            4 | 8 | 128,
            128,
            128,
        ],
        high: [
            8 | 128,
            128,
            1 | 16 | 32,
            2,
            0,
            4 | 64,
            0,
            4,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
    };
    const UTF8_POLICY: Utf8Policy = Utf8Policy::Validate;
    const CLASS_BITS: [u8; 8] = [
        OPERATOR_BITS,
        JSON_WHITESPACE_BITS,
        QUOTE_BITS,
        BACKSLASH_BITS,
        CONTROL_BITS,
        0,
        0,
        0,
    ];
    const CLASS_COUNT: usize = 5;
    const EXACT_BYTES: [u8; 4] = [b'"', b'\\', b':', b','];
    const EXACT_COUNT: usize = 0;

    fn scan_block(
        _input: &[u8; 64],
        base: u32,
        valid: u64,
        masks: &ClassifiedBlock,
        state: &mut Self::Stage1State,
        tape: &mut Vec<u32>,
    ) -> Result<(), Self::Error> {
        let operators = masks.class[0] & valid;
        let whitespace = masks.class[1] & valid;
        let quotes = masks.class[2] & valid;
        let backslashes = masks.class[3] & valid;
        let controls = masks.class[4] & valid;
        let lanes = valid.count_ones();

        // Mark bytes escaped by odd-length backslash runs. Work is proportional
        // to the number of runs, rather than to the 64 lanes in the block.
        let mut escaped_characters = u64::from(state.escaped);
        let mut remaining_backslashes = backslashes & !escaped_characters;
        let mut escapes_next = false;
        while remaining_backslashes != 0 {
            let start = remaining_backslashes.trailing_zeros();
            let run = (remaining_backslashes >> start).trailing_ones();
            let end = start + run;
            let run_mask = if run == 64 {
                u64::MAX
            } else {
                ((1u64 << run) - 1) << start
            };
            remaining_backslashes &= !run_mask;
            if run & 1 != 0 {
                if end < lanes {
                    escaped_characters |= 1u64 << end;
                } else {
                    escapes_next = true;
                }
            }
        }

        let unescaped_quotes = quotes & !escaped_characters;
        let quote_parity = prefix_xor(unescaped_quotes);
        let after_quote_state = if state.in_string {
            !quote_parity
        } else {
            quote_parity
        };
        let string_bytes = after_quote_state & !unescaped_quotes & valid;
        let outside = !string_bytes & !unescaped_quotes & valid;
        let outside_operators = operators & outside;
        let outside_whitespace = whitespace & outside;
        let opening_quotes = unescaped_quotes & after_quote_state & valid;

        let possible_controls = controls & string_bytes & !escaped_characters;
        if possible_controls != 0 {
            return Err(error(
                JsonErrorKind::ControlCharacter,
                base + possible_controls.trailing_zeros(),
            ));
        }

        let delimiters = outside_operators | outside_whitespace;
        let predecessor_delimiter = (delimiters << 1) | u64::from(state.previous_is_delimiter);
        let ordinary = outside & !operators & !whitespace;
        let scalar_starts = ordinary & predecessor_delimiter;
        flatten_offsets(
            outside_operators | opening_quotes | scalar_starts,
            base,
            tape,
        );

        state.in_string ^= unescaped_quotes.count_ones() & 1 != 0;
        state.escaped = state.in_string && escapes_next;
        if lanes != 0 {
            let last = 1u64 << (lanes - 1);
            if state.in_string {
                state.previous_is_delimiter = false;
            } else {
                state.previous_is_delimiter = delimiters & last != 0;
            }
        }
        Ok(())
    }

    fn finish_stage1(input_len: u32, state: &mut Self::Stage1State) -> Result<(), Self::Error> {
        if state.in_string || state.escaped {
            Err(error(JsonErrorKind::UnexpectedEof, input_len))
        } else {
            Ok(())
        }
    }

    fn parse<'a, F>(
        input: &'a [u8],
        _tape: &[u32],
        config: &Self::Config,
        state: &mut Self::Stage2State,
        mut sink: F,
    ) -> Result<(), Self::Error>
    where
        F: FnMut(Self::Event<'a>),
    {
        state.frames.clear();
        let mut cursor = skip_ws(input, 0);
        if cursor == input.len() {
            return Err(error(JsonErrorKind::UnexpectedEof, cursor as u32));
        }
        parse_value(input, &mut cursor, config, state, &mut sink)?;
        cursor = skip_ws(input, cursor);
        if cursor != input.len() {
            return Err(error(JsonErrorKind::TrailingData, cursor as u32));
        }
        Ok(())
    }

    fn input_too_large_error() -> Self::Error {
        error(JsonErrorKind::InputTooLarge, u32::MAX)
    }

    fn capacity_error(offset: u32) -> Self::Error {
        error(JsonErrorKind::Capacity, offset)
    }

    fn utf8_error(offset: u32) -> Self::Error {
        error(JsonErrorKind::Utf8, offset)
    }

    fn reserve_stage2(
        config: &Self::Config,
        state: &mut Self::Stage2State,
        _fallible: bool,
    ) -> Result<(), CapacityError> {
        let additional = config.max_depth.saturating_sub(state.frames.capacity());
        state
            .frames
            .try_reserve_exact(additional)
            .map_err(|_| CapacityError::AllocationFailed {
                requested: config.max_depth,
            })
    }
}

fn parse_value<'a, F>(
    input: &'a [u8],
    cursor: &mut usize,
    config: &JsonConfig,
    state: &mut JsonStage2State,
    sink: &mut F,
) -> Result<(), JsonError>
where
    F: FnMut(JsonEvent<'a>),
{
    if *cursor >= input.len() {
        return Err(error(JsonErrorKind::UnexpectedEof, *cursor as u32));
    }

    match input[*cursor] {
        b'{' => parse_object(input, cursor, config, state, sink),
        b'[' => parse_array(input, cursor, config, state, sink),
        b'"' => {
            let raw = parse_string(input, cursor)?;
            sink(JsonEvent::String { raw });
            Ok(())
        }
        b't' => {
            let token = parse_literal(input, cursor, b"true")?;
            sink(JsonEvent::Boolean {
                value: true,
                span: token,
            });
            Ok(())
        }
        b'f' => {
            let token = parse_literal(input, cursor, b"false")?;
            sink(JsonEvent::Boolean {
                value: false,
                span: token,
            });
            Ok(())
        }
        b'n' => {
            let token = parse_literal(input, cursor, b"null")?;
            sink(JsonEvent::Null { span: token });
            Ok(())
        }
        b'-' | b'0'..=b'9' => {
            let (raw, token_span) = parse_number(input, cursor)?;
            sink(JsonEvent::Number {
                raw,
                span: token_span,
            });
            Ok(())
        }
        _ => Err(error(JsonErrorKind::Structure, *cursor as u32)),
    }
}

fn parse_object<'a, F>(
    input: &'a [u8],
    cursor: &mut usize,
    config: &JsonConfig,
    state: &mut JsonStage2State,
    sink: &mut F,
) -> Result<(), JsonError>
where
    F: FnMut(JsonEvent<'a>),
{
    enter_container(state, config, Container::Object, *cursor)?;
    let start = *cursor;
    *cursor += 1;
    sink(JsonEvent::StartObject {
        offset: start as u32,
    });
    *cursor = skip_ws(input, *cursor);

    if consume(input, cursor, b'}') {
        state.frames.pop();
        sink(JsonEvent::EndObject {
            offset: (*cursor - 1) as u32,
        });
        return Ok(());
    }

    loop {
        if input.get(*cursor) != Some(&b'"') {
            return Err(if *cursor == input.len() {
                error(JsonErrorKind::UnexpectedEof, *cursor as u32)
            } else {
                error(JsonErrorKind::Structure, *cursor as u32)
            });
        }
        let key = parse_string(input, cursor)?;
        sink(JsonEvent::ObjectKey { raw: key });
        *cursor = skip_ws(input, *cursor);
        if !consume(input, cursor, b':') {
            return Err(if *cursor == input.len() {
                error(JsonErrorKind::UnexpectedEof, *cursor as u32)
            } else {
                error(JsonErrorKind::Structure, *cursor as u32)
            });
        }
        *cursor = skip_ws(input, *cursor);
        parse_value(input, cursor, config, state, sink)?;
        *cursor = skip_ws(input, *cursor);

        if consume(input, cursor, b'}') {
            state.frames.pop();
            sink(JsonEvent::EndObject {
                offset: (*cursor - 1) as u32,
            });
            return Ok(());
        }
        if !consume(input, cursor, b',') {
            return Err(if *cursor == input.len() {
                error(JsonErrorKind::UnexpectedEof, *cursor as u32)
            } else {
                error(JsonErrorKind::Structure, *cursor as u32)
            });
        }
        *cursor = skip_ws(input, *cursor);
    }
}

fn parse_array<'a, F>(
    input: &'a [u8],
    cursor: &mut usize,
    config: &JsonConfig,
    state: &mut JsonStage2State,
    sink: &mut F,
) -> Result<(), JsonError>
where
    F: FnMut(JsonEvent<'a>),
{
    enter_container(state, config, Container::Array, *cursor)?;
    let start = *cursor;
    *cursor += 1;
    sink(JsonEvent::StartArray {
        offset: start as u32,
    });
    *cursor = skip_ws(input, *cursor);

    if consume(input, cursor, b']') {
        state.frames.pop();
        sink(JsonEvent::EndArray {
            offset: (*cursor - 1) as u32,
        });
        return Ok(());
    }

    loop {
        parse_value(input, cursor, config, state, sink)?;
        *cursor = skip_ws(input, *cursor);
        if consume(input, cursor, b']') {
            state.frames.pop();
            sink(JsonEvent::EndArray {
                offset: (*cursor - 1) as u32,
            });
            return Ok(());
        }
        if !consume(input, cursor, b',') {
            return Err(if *cursor == input.len() {
                error(JsonErrorKind::UnexpectedEof, *cursor as u32)
            } else {
                error(JsonErrorKind::Structure, *cursor as u32)
            });
        }
        *cursor = skip_ws(input, *cursor);
    }
}

fn enter_container(
    state: &mut JsonStage2State,
    config: &JsonConfig,
    container: Container,
    offset: usize,
) -> Result<(), JsonError> {
    if state.frames.len() >= config.max_depth {
        return Err(error(JsonErrorKind::Depth, offset as u32));
    }
    state.frames.push(container);
    Ok(())
}

fn parse_string<'a>(input: &'a [u8], cursor: &mut usize) -> Result<RawJsonString<'a>, JsonError> {
    let opening = *cursor;
    let start = opening + 1;
    let mut position = start;

    while position < input.len() {
        let relative = input[position..]
            .iter()
            .position(|&byte| byte == b'"' || byte == b'\\' || byte < 0x20);
        position = match relative {
            Some(relative) => position + relative,
            None => return Err(error(JsonErrorKind::UnexpectedEof, input.len() as u32)),
        };

        match input[position] {
            b'"' => {
                *cursor = position + 1;
                return Ok(RawJsonString {
                    raw: &input[start..position],
                    span: span(start, position),
                });
            }
            0x00..=0x1f => {
                return Err(error(JsonErrorKind::ControlCharacter, position as u32));
            }
            b'\\' => {
                position += 1;
                let escaped = *input
                    .get(position)
                    .ok_or_else(|| error(JsonErrorKind::UnexpectedEof, input.len() as u32))?;
                match escaped {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => position += 1,
                    b'u' => {
                        let (unit, next) = parse_hex_unit(input, position + 1)?;
                        position = next;
                        if (0xd800..=0xdbff).contains(&unit) {
                            if input.get(position..position + 2) != Some(b"\\u") {
                                return Err(error(
                                    JsonErrorKind::InvalidSurrogate,
                                    position as u32,
                                ));
                            }
                            let (low, after_low) = parse_hex_unit(input, position + 2)?;
                            if !(0xdc00..=0xdfff).contains(&low) {
                                return Err(error(
                                    JsonErrorKind::InvalidSurrogate,
                                    (position + 2) as u32,
                                ));
                            }
                            position = after_low;
                        } else if (0xdc00..=0xdfff).contains(&unit) {
                            return Err(error(
                                JsonErrorKind::InvalidSurrogate,
                                (position - 4) as u32,
                            ));
                        }
                    }
                    _ => return Err(error(JsonErrorKind::InvalidEscape, position as u32)),
                }
            }
            _ => unreachable!(),
        }
    }

    Err(error(JsonErrorKind::UnexpectedEof, input.len() as u32))
}

fn parse_hex_unit(input: &[u8], start: usize) -> Result<(u16, usize), JsonError> {
    if start + 4 > input.len() {
        return Err(error(JsonErrorKind::UnexpectedEof, input.len() as u32));
    }
    let mut value = 0u16;
    for (position, &byte) in input.iter().enumerate().skip(start).take(4) {
        value = (value << 4)
            | hex(byte).ok_or_else(|| error(JsonErrorKind::InvalidEscape, position as u32))?;
    }
    Ok((value, start + 4))
}

fn parse_literal(input: &[u8], cursor: &mut usize, literal: &[u8]) -> Result<Span, JsonError> {
    let start = *cursor;
    if input.get(start..start + literal.len()) != Some(literal) {
        return Err(error(JsonErrorKind::InvalidLiteral, start as u32));
    }
    let end = start + literal.len();
    if input
        .get(end)
        .is_some_and(|&byte| !is_value_delimiter(byte))
    {
        return Err(error(JsonErrorKind::InvalidLiteral, end as u32));
    }
    *cursor = end;
    Ok(span(start, end))
}

fn parse_number<'a>(input: &'a [u8], cursor: &mut usize) -> Result<(&'a [u8], Span), JsonError> {
    let start = *cursor;
    let mut position = start;

    if input[position] == b'-' {
        position += 1;
        if position == input.len() {
            return Err(error(JsonErrorKind::InvalidNumber, position as u32));
        }
    }

    match input.get(position) {
        Some(b'0') => {
            position += 1;
            if input.get(position).is_some_and(u8::is_ascii_digit) {
                return Err(error(JsonErrorKind::InvalidNumber, position as u32));
            }
        }
        Some(b'1'..=b'9') => {
            position += 1;
            while input.get(position).is_some_and(u8::is_ascii_digit) {
                position += 1;
            }
        }
        _ => return Err(error(JsonErrorKind::InvalidNumber, position as u32)),
    }

    if input.get(position) == Some(&b'.') {
        position += 1;
        let fraction_start = position;
        while input.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if position == fraction_start {
            return Err(error(JsonErrorKind::InvalidNumber, position as u32));
        }
    }

    if matches!(input.get(position), Some(b'e' | b'E')) {
        position += 1;
        if matches!(input.get(position), Some(b'+' | b'-')) {
            position += 1;
        }
        let exponent_start = position;
        while input.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if position == exponent_start {
            return Err(error(JsonErrorKind::InvalidNumber, position as u32));
        }
    }

    if input
        .get(position)
        .is_some_and(|&byte| !is_value_delimiter(byte))
    {
        return Err(error(JsonErrorKind::InvalidNumber, position as u32));
    }

    *cursor = position;
    Ok((&input[start..position], span(start, position)))
}

#[inline]
fn is_value_delimiter(byte: u8) -> bool {
    is_json_ws(byte) || matches!(byte, b',' | b']' | b'}')
}

#[inline]
fn skip_ws(input: &[u8], mut cursor: usize) -> usize {
    while input.get(cursor).is_some_and(|&byte| is_json_ws(byte)) {
        cursor += 1;
    }
    cursor
}

#[inline]
fn consume(input: &[u8], cursor: &mut usize, wanted: u8) -> bool {
    if input.get(*cursor) == Some(&wanted) {
        *cursor += 1;
        true
    } else {
        false
    }
}

#[inline]
fn is_json_ws(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

#[inline]
fn hex(byte: u8) -> Option<u16> {
    match byte {
        b'0'..=b'9' => Some((byte - b'0') as u16),
        b'a'..=b'f' => Some((byte - b'a' + 10) as u16),
        b'A'..=b'F' => Some((byte - b'A' + 10) as u16),
        _ => None,
    }
}

#[inline]
fn error(kind: JsonErrorKind, offset: u32) -> JsonError {
    JsonError { kind, offset }
}

#[inline]
fn prefix_xor(mut mask: u64) -> u64 {
    mask ^= mask << 1;
    mask ^= mask << 2;
    mask ^= mask << 4;
    mask ^= mask << 8;
    mask ^= mask << 16;
    mask ^= mask << 32;
    mask
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

/// Decode a validated raw JSON string into reusable caller-owned storage.
///
/// `raw` excludes the surrounding quotes but retains JSON escape sequences.
/// The scratch vector is cleared, not replaced, so its capacity is reusable.
pub fn decode_json_string<'scratch>(
    raw: RawJsonString<'_>,
    scratch: &'scratch mut Vec<u8>,
) -> Result<&'scratch str, JsonError> {
    scratch.clear();
    let input = raw.raw;
    let mut cursor = 0usize;

    while cursor < input.len() {
        if input[cursor] != b'\\' {
            let run_start = cursor;
            while cursor < input.len() && input[cursor] != b'\\' {
                cursor += 1;
            }
            scratch.extend_from_slice(&input[run_start..cursor]);
            continue;
        }

        cursor += 1;
        let escaped = *input.get(cursor).ok_or_else(|| {
            error(
                JsonErrorKind::UnexpectedEof,
                raw.span.start.saturating_add(cursor as u32),
            )
        })?;
        match escaped {
            b'"' | b'\\' | b'/' => scratch.push(escaped),
            b'b' => scratch.push(0x08),
            b'f' => scratch.push(0x0c),
            b'n' => scratch.push(b'\n'),
            b'r' => scratch.push(b'\r'),
            b't' => scratch.push(b'\t'),
            b'u' => {
                let (high, next) = parse_hex_unit_relative(input, cursor + 1, raw.span.start)?;
                cursor = next - 1;
                let scalar = if (0xd800..=0xdbff).contains(&high) {
                    if input.get(next..next + 2) != Some(b"\\u") {
                        return Err(error(
                            JsonErrorKind::InvalidSurrogate,
                            raw.span.start.saturating_add(next as u32),
                        ));
                    }
                    let (low, after) = parse_hex_unit_relative(input, next + 2, raw.span.start)?;
                    if !(0xdc00..=0xdfff).contains(&low) {
                        return Err(error(
                            JsonErrorKind::InvalidSurrogate,
                            raw.span.start.saturating_add((next + 2) as u32),
                        ));
                    }
                    cursor = after - 1;
                    0x1_0000 + (((high as u32 - 0xd800) << 10) | (low as u32 - 0xdc00))
                } else if (0xdc00..=0xdfff).contains(&high) {
                    return Err(error(
                        JsonErrorKind::InvalidSurrogate,
                        raw.span.start.saturating_add((next - 4) as u32),
                    ));
                } else {
                    high as u32
                };
                let character = char::from_u32(scalar).ok_or_else(|| {
                    error(
                        JsonErrorKind::InvalidSurrogate,
                        raw.span.start.saturating_add(cursor as u32),
                    )
                })?;
                let mut encoded = [0u8; 4];
                scratch.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            }
            _ => {
                return Err(error(
                    JsonErrorKind::InvalidEscape,
                    raw.span.start.saturating_add(cursor as u32),
                ));
            }
        }
        cursor += 1;
    }

    std::str::from_utf8(scratch).map_err(|utf8| {
        error(
            JsonErrorKind::Utf8,
            raw.span.start.saturating_add(utf8.valid_up_to() as u32),
        )
    })
}

fn parse_hex_unit_relative(
    input: &[u8],
    start: usize,
    base: u32,
) -> Result<(u16, usize), JsonError> {
    if start + 4 > input.len() {
        return Err(error(
            JsonErrorKind::UnexpectedEof,
            base.saturating_add(input.len() as u32),
        ));
    }
    let mut value = 0u16;
    for (position, &byte) in input.iter().enumerate().skip(start).take(4) {
        value = (value << 4)
            | hex(byte).ok_or_else(|| {
                error(
                    JsonErrorKind::InvalidEscape,
                    base.saturating_add(position as u32),
                )
            })?;
    }
    Ok((value, start + 4))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BackendKind, Parser};

    #[test]
    fn table_is_exact_for_named_sets() {
        for byte in u8::MIN..=u8::MAX {
            let class = JsonGrammar::TABLES.classify(byte);
            assert_eq!(
                class & OPERATOR_BITS != 0,
                matches!(byte, b'{' | b'}' | b'[' | b']' | b',' | b':')
            );
            assert_eq!(class & JSON_WHITESPACE_BITS != 0, is_json_ws(byte));
            assert_eq!(class & QUOTE_BITS != 0, byte == b'"');
            assert_eq!(class & BACKSLASH_BITS != 0, byte == b'\\');
            assert_eq!(class & CONTROL_BITS != 0, byte < 0x20);
        }
    }

    #[test]
    fn validates_and_emits_zero_copy_events() {
        let input = br#"{"name":"a\nb","n":-12.5e+2,"ok":true,"none":null,"a":[1]}"#;
        let mut parser =
            Parser::<JsonGrammar>::with_backend(JsonConfig::default(), BackendKind::Scalar)
                .unwrap();
        let mut events = Vec::new();
        parser
            .parse_with(input, |event| events.push(event))
            .unwrap();

        assert!(events.contains(&JsonEvent::String {
            raw: RawJsonString {
                raw: b"a\\nb",
                span: Span { start: 9, end: 13 },
            }
        }));
        assert!(events.iter().any(|event| matches!(
            event,
            JsonEvent::Number { raw, .. } if *raw == b"-12.5e+2"
        )));
    }

    #[test]
    fn rejects_bad_numbers_escapes_surrogates_and_structure() {
        for input in [
            b"[01]".as_slice(),
            br#"["\x"]"#,
            br#"["\uD800"]"#,
            br#"{"x",1}"#,
            br#"[1,]"#,
            b"true false",
        ] {
            let mut parser = Parser::<JsonGrammar>::new(JsonConfig::default());
            assert!(parser.parse_with(input, |_| {}).is_err(), "{input:?}");
        }
    }

    #[test]
    fn decode_uses_caller_scratch() {
        let raw = RawJsonString {
            raw: br#"a\n\uD83D\uDE03"#,
            span: Span { start: 1, end: 18 },
        };
        let mut scratch = Vec::new();
        assert_eq!(decode_json_string(raw, &mut scratch).unwrap(), "a\n😃");
    }

    #[test]
    fn acceptance_matches_serde_json_on_reference_cases() {
        let cases: &[&[u8]] = &[
            b"null",
            b" true ",
            b"-0.125e-2",
            br#""""#,
            r#"{"a":[],"b":{},"unicode":"é雪","pair":"\uD83D\uDE03"}"#.as_bytes(),
            b"",
            b"+1",
            b"01",
            b"1.",
            b"1e",
            br#"{"a":1,}"#,
            br#"["\uDEAD"]"#,
            br#"["line
break"]"#,
            b"true false",
            b"[1 2]",
        ];

        for &input in cases {
            let expected = serde_json::from_slice::<serde_json::Value>(input).is_ok();
            let mut parser = Parser::<JsonGrammar>::new(JsonConfig::default());
            let actual = parser.parse_with(input, |_| {}).is_ok();
            assert_eq!(actual, expected, "acceptance mismatch for {input:?}");
        }
    }

    #[test]
    fn enforces_configured_depth() {
        let mut parser = Parser::<JsonGrammar>::new(JsonConfig { max_depth: 2 });
        parser.parse_with(b"[[]]", |_| {}).unwrap();
        let error = parser.parse_with(b"[[[]]]", |_| {}).unwrap_err();
        assert_eq!(error.kind, JsonErrorKind::Depth);
        assert_eq!(error.offset, 2);
    }

    #[test]
    fn differential_acceptance_for_short_inputs() {
        const ALPHABET: &[u8] = b"{}[],:\"\\-01atfn \n";
        for length in 0..=3 {
            let combinations = ALPHABET.len().pow(length);
            for mut encoded in 0..combinations {
                let mut input = vec![0u8; length as usize];
                for byte in &mut input {
                    *byte = ALPHABET[encoded % ALPHABET.len()];
                    encoded /= ALPHABET.len();
                }
                let expected = serde_json::from_slice::<serde_json::Value>(&input).is_ok();
                let mut parser = Parser::<JsonGrammar>::new(JsonConfig::default());
                let actual = parser.parse_with(&input, |_| {}).is_ok();
                assert_eq!(actual, expected, "acceptance mismatch for {input:?}");
            }
        }
    }

    #[test]
    fn escape_runs_and_utf8_cross_block_boundaries() {
        for boundary in 60..68 {
            let mut input = Vec::new();
            input.extend_from_slice(br#"{"x":""#);
            input.extend(std::iter::repeat_n(b'a', boundary - input.len()));
            input.extend_from_slice(br#"\\\"quoted","unicode":"#);
            input.push(b'"');
            input.extend_from_slice("雪".as_bytes());
            input.extend_from_slice(br#""}"#);

            let mut parser = Parser::<JsonGrammar>::new(JsonConfig::default());
            parser
                .parse_with(&input, |_| {})
                .unwrap_or_else(|error| panic!("boundary {boundary}: {error}; {input:?}"));
            assert!(serde_json::from_slice::<serde_json::Value>(&input).is_ok());
        }
    }
}
