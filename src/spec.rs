/// The two 16-byte lookup tables executed by every backend.
///
/// A byte is classified by intersecting `low[byte & 15]` and
/// `high[byte >> 4]`. Specs assign meanings to the resulting eight bits.
/// Tables can be written by hand or emitted by [`crate::tablegen`].
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

    const TABLES: NibbleTables;
    /// Up to eight bit selectors applied to [`Self::TABLES`].
    const CLASS_BITS: [u8; 8];
    const CLASS_COUNT: usize;
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
        Ok(masks.union(Self::MASK_CLASSES..Self::CLASS_COUNT))
    }

    /// Called once after the block containing the end of input.
    #[inline]
    fn finish(_input_len: u32, _state: &mut Self::State) -> Result<(), Self::Error> {
        Ok(())
    }
}
