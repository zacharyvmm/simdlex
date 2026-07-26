//! Compile-time grammar contracts used by [`crate::Parser`].

mod html;

#[cfg(any(feature = "json-test-grammar", test))]
mod json;

pub use html::{
    HtmlConfig, HtmlEmit, HtmlError, HtmlErrorKind, HtmlEvent, HtmlGrammar, HtmlStage1State,
};

#[cfg(any(feature = "json-test-grammar", test))]
pub use json::{
    JsonConfig, JsonError, JsonErrorKind, JsonEvent, JsonGrammar, RawJsonString, decode_json_string,
};

use crate::{CapacityError, ClassifiedBlock, Span};

/// The two 16-byte lookup tables used by scalar and SIMD byte classifiers.
///
/// A byte is classified by intersecting `low[byte & 15]` and
/// `high[byte >> 4]`. Grammars assign meanings to the resulting eight bits.
///
/// A future table generator may emit these constants from named byte sets. Its
/// contract is to exhaustively reject false positives and class mismatches for
/// all 256 byte values, and to fail at compile time when mutually exclusive
/// domains cannot fit in eight bits. Hand-written tables use that same format,
/// so generation will not require a backend or parser API change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NibbleTables {
    pub low: [u8; 16],
    pub high: [u8; 16],
}

impl NibbleTables {
    #[inline]
    pub const fn classify(self, byte: u8) -> u8 {
        self.low[(byte & 0x0f) as usize] & self.high[(byte >> 4) as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Utf8Policy {
    ByteOriented,
    Validate,
}

/// A grammar implemented as monomorphized stage-1 and stage-2 hooks.
///
/// `CLASS_BITS` contains up to eight masks selected from [`Self::TABLES`].
/// Backends only implement nibble lookup, bit selection, exact comparison
/// primitives, and 64-lane mask assembly; they contain no grammar-specific
/// byte constants.
pub trait Grammar: Sized {
    type Config;
    type Error;
    type Event<'a>: Copy
    where
        Self: 'a;
    type Stage1State: Default;
    type Stage2State: Default;

    const TABLES: NibbleTables;
    const UTF8_POLICY: Utf8Policy;
    const CLASS_BITS: [u8; 8];
    const CLASS_COUNT: usize;
    const EXACT_BYTES: [u8; 4];
    const EXACT_COUNT: usize;

    fn scan_block(
        input: &[u8; 64],
        base: u32,
        valid: u64,
        masks: &ClassifiedBlock,
        state: &mut Self::Stage1State,
        tape: &mut Vec<u32>,
    ) -> Result<(), Self::Error>;

    fn finish_stage1(input_len: u32, state: &mut Self::Stage1State) -> Result<(), Self::Error>;

    fn parse<'a, F>(
        input: &'a [u8],
        tape: &[u32],
        config: &Self::Config,
        state: &mut Self::Stage2State,
        sink: F,
    ) -> Result<(), Self::Error>
    where
        F: FnMut(Self::Event<'a>),
        Self: 'a;

    fn input_too_large_error() -> Self::Error;
    fn capacity_error(offset: u32) -> Self::Error;
    fn utf8_error(offset: u32) -> Self::Error;

    fn reserve_stage2(
        config: &Self::Config,
        state: &mut Self::Stage2State,
        fallible: bool,
    ) -> Result<(), CapacityError>;
}

#[inline]
pub(crate) fn span(start: usize, end: usize) -> Span {
    Span {
        start: start as u32,
        end: end as u32,
    }
}
