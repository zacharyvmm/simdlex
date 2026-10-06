//! A YAML event stream: a hybrid-index stage 1 and a streaming stage 2.
//!
//! The index only records the bounds stage 2 has to search for. In YAML
//! those are line feeds and spaces, which bound every line and its
//! indentation, and quotes, which bound scalars that may span many lines.
//! Everything else (`:`, `#`, `-`, flow indicators) lies inside a line that
//! stage 2 is already walking, so it is read in place rather than indexed.
//!
//! Line feeds and spaces are dense, so they stay per-block masks; quotes are
//! sparse, so they go to a tape ([`IndexMode::Hybrid`]). Stage 2 measures
//! indentation with [`simdlex_index::MaskView::run_length`], finds line ends
//! with [`simdlex_index::MaskView::next_in`], and jumps along the tape to
//! closing quotes.
//!
//! Stage 2 emits libyaml-style events and builds no tree. Scalars are
//! borrowed raw spans; [`decode_yaml_scalar`] folds and unescapes them into
//! caller-owned scratch storage when needed. See `docs/yaml-stream.md` for
//! the supported subset.

mod decode;
mod stage2;
#[cfg(test)]
mod tests;

use std::fmt;

use simdlex_index::{ClassifiedBlock, IndexMode, IndexSpec, IndexView, NibbleTables, Utf8Policy};
use simdlex_tablegen::{ByteSet, Tables, generate, unwrap};

use crate::grammar::Grammar;
use crate::{CapacityError, Span};

pub use decode::decode_yaml_scalar;

/// Class indices. Layout classes come first: they keep their masks in
/// hybrid mode.
pub(crate) const LINE_FEED: usize = 0;
pub(crate) const SPACE: usize = 1;
/// Quotes, recorded on the tape in hybrid mode. A quoted scalar can span
/// many lines, so stage 2 jumps to its closing quote through the index.
/// Everything else (`:`, `#`, `-`, flow indicators) is found by scanning
/// the current line, which the line-feed mask already bounds.
pub(crate) const QUOTES: usize = 2;

const CLASSES: [ByteSet; 3] = [
    ByteSet::from_bytes(b"\n"),
    ByteSet::from_bytes(b" "),
    ByteSet::from_bytes(b"'\""),
];

const GENERATED: Tables<3> = unwrap(generate(&CLASSES));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YamlConfig {
    /// Maximum nesting of block and flow collections.
    pub max_depth: usize,
}

impl Default for YamlConfig {
    fn default() -> Self {
        Self { max_depth: 1_024 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YamlEvent<'a> {
    DocumentStart {
        /// Whether the document began with `---`.
        explicit: bool,
        offset: u32,
    },
    DocumentEnd {
        /// Whether the document ended with `...`.
        explicit: bool,
        offset: u32,
    },
    SequenceStart {
        style: YamlCollectionStyle,
        properties: YamlProperties<'a>,
        offset: u32,
    },
    SequenceEnd {
        offset: u32,
    },
    MappingStart {
        style: YamlCollectionStyle,
        properties: YamlProperties<'a>,
        offset: u32,
    },
    MappingEnd {
        offset: u32,
    },
    Scalar(YamlScalar<'a>),
    Alias {
        name: &'a [u8],
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YamlCollectionStyle {
    Block,
    Flow,
}

/// An anchor (`&name`) and tag (`!tag`) attached to a node, as written.
/// Tag handles are not resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct YamlProperties<'a> {
    pub anchor: Option<&'a [u8]>,
    pub tag: Option<&'a [u8]>,
}

impl YamlProperties<'_> {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.anchor.is_none() && self.tag.is_none()
    }
}

/// A scalar as written. `raw` excludes quotes and block-scalar headers but is
/// otherwise unprocessed: line folding, escapes, and block indentation are
/// applied by [`decode_yaml_scalar`]. An empty plain scalar is an empty node
/// (YAML null), such as the value in `key:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YamlScalar<'a> {
    pub raw: &'a [u8],
    pub span: Span,
    pub style: YamlScalarStyle,
    pub properties: YamlProperties<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YamlScalarStyle {
    Plain,
    SingleQuoted,
    DoubleQuoted,
    /// `|`. `raw` spans the content lines, indentation included.
    Literal(YamlBlockScalar),
    /// `>`. `raw` spans the content lines, indentation included.
    Folded(YamlBlockScalar),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YamlBlockScalar {
    /// Content indentation, explicit or detected.
    pub indent: u32,
    pub chomping: YamlChomping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YamlChomping {
    /// `-`: drop the final line break and trailing empty lines.
    Strip,
    /// Default: keep the final line break, drop trailing empty lines.
    Clip,
    /// `+`: keep the final line break and trailing empty lines.
    Keep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YamlError {
    pub kind: YamlErrorKind,
    pub offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YamlErrorKind {
    Utf8,
    InputTooLarge,
    Capacity,
    Depth,
    /// Stage 2 needs layout masks ([`IndexMode::Hybrid`] or
    /// [`IndexMode::Masks`]); a tape alone omits indentation.
    RequiresMaskIndex,
    /// A tab where indentation is required.
    TabIndentation,
    /// Content indented inconsistently with its collection.
    BadIndentation,
    /// Content that cannot appear where it was found, such as a second root
    /// node or `key: a: b`.
    UnexpectedContent,
    UnterminatedQuote,
    UnterminatedFlow,
    InvalidBlockScalarHeader,
    /// A plain scalar starting with a reserved indicator.
    InvalidPlainScalar,
    InvalidEscape,
    /// Valid YAML outside the supported subset (explicit `?` keys, empty
    /// keys, complex keys, single-pair mappings in flow sequences).
    Unsupported,
}

impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} at byte {}", self.kind, self.offset)
    }
}

impl std::error::Error for YamlError {}

#[inline]
pub(crate) fn error(kind: YamlErrorKind, offset: usize) -> YamlError {
    YamlError {
        kind,
        offset: offset as u32,
    }
}

/// Stage-2 frames: open block and flow collections.
#[derive(Debug, Default)]
pub struct YamlStage2State {
    frames: Vec<stage2::Frame>,
}

#[derive(Debug, Clone, Copy)]
pub struct YamlGrammar;

impl IndexSpec for YamlGrammar {
    type State = ();
    type Error = YamlError;

    const TABLES: NibbleTables = NibbleTables {
        low: GENERATED.low,
        high: GENERATED.high,
    };
    const CLASS_BITS: [u8; 8] = GENERATED.padded_class_bits();
    const CLASS_COUNT: usize = CLASSES.len();
    const EXACT_BYTES: [u8; 4] = [0; 4];
    const EXACT_COUNT: usize = 0;
    const UTF8_POLICY: Utf8Policy = Utf8Policy::Validate;
    // Spaces and line feeds are dense, so they stay masks; quotes are
    // sparse, so they are cheaper as a tape.
    const MODE: IndexMode = IndexMode::Hybrid;
    const HYBRID_MASK_CLASSES: usize = QUOTES;

    #[inline]
    fn scan_block(
        _input: &[u8; 64],
        _base: u32,
        _valid: u64,
        masks: &mut ClassifiedBlock,
        _state: &mut (),
    ) -> Result<u64, YamlError> {
        // Quotes are the structurals. Quote context cannot be resolved here:
        // a `'` inside a plain scalar (`don't`) is ordinary text, so
        // prefix-XOR parity would be wrong.
        Ok(masks.class[QUOTES])
    }

    #[inline]
    fn finish(_input_len: u32, _state: &mut ()) -> Result<(), YamlError> {
        Ok(())
    }

    fn utf8_error(offset: u32) -> YamlError {
        YamlError {
            kind: YamlErrorKind::Utf8,
            offset,
        }
    }
}

impl Grammar for YamlGrammar {
    type Config = YamlConfig;
    type Event<'a> = YamlEvent<'a>;
    type Stage2State = YamlStage2State;

    fn parse<'a, F>(
        input: &'a [u8],
        index: IndexView<'_>,
        config: &Self::Config,
        state: &mut Self::Stage2State,
        sink: F,
    ) -> Result<(), Self::Error>
    where
        F: FnMut(Self::Event<'a>),
    {
        let (masks, quotes) = match index {
            IndexView::Hybrid { tape, masks } => (masks, stage2::Quotes::tape(tape)),
            IndexView::Masks(masks) => (masks, stage2::Quotes::Masks(masks)),
            IndexView::Tape(_) => return Err(error(YamlErrorKind::RequiresMaskIndex, 0)),
        };
        state.frames.clear();
        stage2::Stage2::new(
            input,
            masks,
            quotes,
            &mut state.frames,
            config.max_depth,
            sink,
        )
        .run()
    }

    fn input_too_large_error() -> Self::Error {
        error(YamlErrorKind::InputTooLarge, u32::MAX as usize)
    }

    fn capacity_error(offset: u32) -> Self::Error {
        error(YamlErrorKind::Capacity, offset as usize)
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
