/// The lookup tables and class selectors executed by every backend.
///
/// A byte is classified by intersecting `low[byte & 15]` and
/// `high[byte >> 4]`; it belongs to class `c` when the result shares a bit
/// with `class_bits[c]`. Tables can be written by hand or generated, and
/// exhaustively checked, at compile time by [`crate::tablegen`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NibbleTables {
    pub low: [u8; 16],
    pub high: [u8; 16],
    /// One bit selector per class. Entries from `class_count` on are zero.
    pub class_bits: [u8; 8],
    /// Number of classes, at most eight.
    pub class_count: usize,
}

impl NibbleTables {
    /// The table bits set for `byte`, before class selection.
    #[inline]
    pub const fn classify(self, byte: u8) -> u8 {
        self.low[(byte & 0x0f) as usize] & self.high[(byte >> 4) as usize]
    }

    /// Number of the eight table bits that some class selects.
    pub const fn bits_used(self) -> u32 {
        let mut used = 0u8;
        let mut class = 0;
        while class < self.class_bits.len() {
            used |= self.class_bits[class];
            class += 1;
        }
        used.count_ones()
    }
}

/// One 64-bit lane mask per class for one 64-byte block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClassifiedBlock {
    pub class: [u64; 8],
}

impl ClassifiedBlock {
    /// Union of the class masks in `classes`.
    #[inline]
    pub fn union(&self, classes: core::ops::Range<usize>) -> u64 {
        self.class[classes]
            .iter()
            .fold(0, |union, mask| union | mask)
    }

    #[inline]
    pub(crate) fn retain(&mut self, valid: u64) {
        for mask in &mut self.class {
            *mask &= valid;
        }
    }
}

/// A compile-time description of one structural scan.
///
/// Backends only perform nibble lookup, bit selection, and 64-lane mask
/// assembly; they contain no grammar-specific byte constants. The index
/// always holds a tape of structural offsets, plus the per-block masks of the
/// first [`Self::MASK_CLASSES`] classes.
///
/// Order the classes so that dense ones (kept as masks, such as YAML's
/// spaces) come first and sparse ones (flattened to the tape, such as quotes)
/// come last.
pub trait IndexSpec: Sized {
    /// Lexical state carried between blocks, such as "inside a string".
    type State: Default;
    type Error;

    /// The byte classes, usually `tablegen::unwrap(tablegen::generate(..))`.
    const TABLES: NibbleTables;
    /// How many leading classes keep their masks per block. With the default
    /// of 0 the index is a tape only, as in simdjson.
    const MASK_CLASSES: usize = 0;

    /// Refine one block and return its structural lanes, which are flattened
    /// onto the tape. On entry, `masks` holds the classified block with lanes
    /// outside `valid` cleared; the first [`Self::MASK_CLASSES`] masks are
    /// stored after this returns, so a spec may refine them too.
    ///
    /// The default makes every class after the mask classes structural.
    #[inline]
    fn scan_block(
        _input: &[u8; 64],
        _base: u32,
        _valid: u64,
        masks: &mut ClassifiedBlock,
        _state: &mut Self::State,
    ) -> Result<u64, Self::Error> {
        Ok(masks.union(Self::MASK_CLASSES..Self::TABLES.class_count))
    }

    /// Called once after the block containing the end of input.
    #[inline]
    fn finish(_input_len: u32, _state: &mut Self::State) -> Result<(), Self::Error> {
        Ok(())
    }
}
