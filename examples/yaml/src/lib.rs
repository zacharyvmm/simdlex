//! A YAML event stream: a hybrid-index stage 1 and a streaming stage 2.
//!
//! The index only records the bounds stage 2 has to search for. In YAML
//! those are line feeds and spaces, which bound every line and its
//! indentation, and quotes, which bound scalars that may span many lines.
//! Everything else (`:`, `#`, `-`, flow indicators) lies inside a line that
//! stage 2 is already walking, so it is read in place rather than indexed.
//!
//! Line feeds and spaces are dense, so they stay per-block masks; quotes are
//! sparse, so they go to the tape. Stage 2 measures
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

use std::convert::Infallible;
use std::fmt;

use simdlex::tablegen::{ByteSet, Tables, generate, unwrap};
use simdlex::{BackendError, BackendKind, IndexSpec, Indexer, NibbleTables, StructuralIndex};

/// A half-open byte range of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

#[inline]
pub(crate) fn span(start: usize, end: usize) -> Span {
    Span {
        start: start as u32,
        end: end as u32,
    }
}

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

/// The YAML structural index: line feeds and spaces as masks, quotes on the
/// tape.
#[derive(Debug, Clone, Copy)]
pub struct YamlSpec;

impl IndexSpec for YamlSpec {
    type State = ();
    type Error = Infallible;

    const TABLES: NibbleTables = NibbleTables {
        low: GENERATED.low,
        high: GENERATED.high,
    };
    const CLASS_BITS: [u8; 8] = GENERATED.padded_class_bits();
    const CLASS_COUNT: usize = CLASSES.len();
    // Spaces and line feeds are dense, so they stay masks. Quotes are the
    // sparse remainder and become the tape (the default `scan_block`).
    // Quote context cannot be resolved here: a `'` inside a plain scalar
    // (`don't`) is ordinary text, so prefix-XOR parity would be wrong.
    const MASK_CLASSES: usize = QUOTES;
}

/// Stage 1 and stage 2 with reusable buffers.
#[derive(Debug)]
pub struct YamlParser {
    config: YamlConfig,
    indexer: Indexer<YamlSpec>,
    index: StructuralIndex,
    frames: Vec<stage2::Frame>,
    max_input: Option<usize>,
}

impl YamlParser {
    pub fn new(config: YamlConfig) -> Self {
        Self::with_backend(config, BackendKind::Auto)
            .expect("the automatically selected backend must be available")
    }

    pub fn with_backend(config: YamlConfig, backend: BackendKind) -> Result<Self, BackendError> {
        let mut parser = Self::unreserved(config, backend)?;
        parser.frames.reserve_exact(parser.config.max_depth);
        Ok(parser)
    }

    /// Reserve for documents of up to `max_input` bytes, so later parses do
    /// not allocate; larger documents are rejected.
    pub fn with_capacity(config: YamlConfig, max_input: usize) -> Result<Self, YamlError> {
        let mut parser = Self::unreserved(config, BackendKind::Auto)
            .expect("the automatically selected backend must be available");
        let capacity_error = |_| error(YamlErrorKind::Capacity, max_input);
        parser
            .frames
            .try_reserve_exact(parser.config.max_depth)
            .map_err(capacity_error)?;
        Indexer::<YamlSpec>::reserve(&mut parser.index, max_input).map_err(capacity_error)?;
        parser.max_input = Some(max_input);
        Ok(parser)
    }

    fn unreserved(config: YamlConfig, backend: BackendKind) -> Result<Self, BackendError> {
        Ok(Self {
            config,
            indexer: Indexer::with_backend(backend)?,
            index: StructuralIndex::new(),
            frames: Vec::new(),
            max_input: None,
        })
    }

    pub fn backend(&self) -> BackendKind {
        self.indexer.backend()
    }

    /// Validate UTF-8 and build the index.
    pub fn stage1(&mut self, input: &[u8]) -> Result<(), YamlError> {
        if input.len() >= u32::MAX as usize {
            return Err(error(YamlErrorKind::InputTooLarge, u32::MAX as usize));
        }
        if let Some(max_input) = self.max_input
            && input.len() > max_input
        {
            return Err(error(YamlErrorKind::Capacity, max_input));
        }
        if let Err(invalid) = std::str::from_utf8(input) {
            return Err(error(YamlErrorKind::Utf8, invalid.valid_up_to()));
        }
        let Ok(()) = self.indexer.index(input, &mut (), &mut self.index);
        Ok(())
    }

    /// The index built by the last [`Self::stage1`].
    pub fn index(&self) -> &StructuralIndex {
        &self.index
    }

    /// Both stages.
    pub fn parse_with<'a>(
        &mut self,
        input: &'a [u8],
        sink: impl FnMut(YamlEvent<'a>),
    ) -> Result<(), YamlError> {
        self.stage1(input)?;
        self.frames.clear();
        stage2::Stage2::new(
            input,
            self.index.masks(),
            stage2::Quotes::new(self.index.tape()),
            &mut self.frames,
            self.config.max_depth,
            sink,
        )
        .run()
    }
}
