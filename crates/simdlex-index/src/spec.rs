/// The two 16-byte lookup tables executed by every backend.
///
/// A byte is classified by intersecting `low[byte & 15]` and
/// `high[byte >> 4]`. Specs assign meanings to the resulting eight bits.
/// Tables can be written by hand or emitted by `simdlex-tablegen`.
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

/// Spec-selected class and exact-byte masks for one logical 64-byte block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClassifiedBlock {
    pub class: [u64; 8],
    pub exact: [u64; 4],
}

impl ClassifiedBlock {
    /// Union of the first `count` class masks.
    #[inline]
    pub fn union_of_classes(&self, count: usize) -> u64 {
        self.class[..count]
            .iter()
            .fold(0, |union, mask| union | mask)
    }

    #[inline]
    pub(crate) fn retain(&mut self, valid: u64) {
        for mask in &mut self.class {
            *mask &= valid;
        }
        for mask in &mut self.exact {
            *mask &= valid;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Utf8Policy {
    ByteOriented,
    Validate,
}

/// How a scan records its results.
///
/// - [`IndexMode::Tape`] flattens each block's structural mask into a global
///   `u32` offset tape, as simdjson does. This suits grammars whose
///   structurals are sparse single-byte delimiters (JSON, HTML tags), because
///   stage 2 then jumps straight from delimiter to delimiter.
/// - [`IndexMode::Masks`] keeps the per-block bitmasks instead. It costs a
///   fixed `8 * stride` bytes per 64 input bytes regardless of density, which
///   wins when significant bytes are dense — for example YAML, where
///   indentation spaces drive the state machine and a tape would hold an
///   entry for most input bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndexMode {
    #[default]
    Tape,
    Masks,
}

/// A compile-time description of one lexical scan.
///
/// Backends only perform nibble lookup, bit selection, exact comparison, and
/// 64-lane mask assembly; they contain no grammar-specific byte constants.
/// [`IndexSpec::scan_block`] is the single grammar hook: it refines the raw
/// masks (for example, removing bytes inside strings) and chooses which lanes
/// are structural.
pub trait IndexSpec: Sized {
    /// Lexical state carried between blocks, such as "inside a string".
    type State: Default;
    type Error;

    const TABLES: NibbleTables;
    /// Up to eight bit selectors applied to [`Self::TABLES`].
    const CLASS_BITS: [u8; 8];
    const CLASS_COUNT: usize;
    /// Up to four bytes compared directly, without the tables.
    const EXACT_BYTES: [u8; 4];
    const EXACT_COUNT: usize;
    const UTF8_POLICY: Utf8Policy;
    /// The mode a parser uses unless the caller overrides it.
    const MODE: IndexMode;

    /// Refine one block. On entry, `masks` holds the classified block with
    /// lanes outside `valid` already cleared. Returns the structural lanes.
    ///
    /// In [`IndexMode::Tape`] the returned lanes are flattened into offsets.
    /// In [`IndexMode::Masks`] they are stored together with the refined
    /// class and exact masks left in `masks`.
    fn scan_block(
        input: &[u8; 64],
        base: u32,
        valid: u64,
        masks: &mut ClassifiedBlock,
        state: &mut Self::State,
    ) -> Result<u64, Self::Error>;

    /// Called once after the last block.
    fn finish(input_len: u32, state: &mut Self::State) -> Result<(), Self::Error>;

    fn utf8_error(offset: u32) -> Self::Error;
}
