use std::collections::TryReserveError;

use crate::IndexMode;

/// Extra tape slots reserved past the input length so flattening may write
/// without per-offset bounds checks.
pub const SAFETY_PADDING: usize = 8;

/// The layout of one block of masks: the structural mask (in
/// [`IndexMode::Masks`] only), followed by `classes` class masks and
/// `exacts` exact-byte masks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MaskLayout {
    /// Whether the structural mask is stored. In [`IndexMode::Hybrid`] the
    /// structural lanes go to the tape instead.
    pub structural: bool,
    pub classes: usize,
    pub exacts: usize,
}

impl MaskLayout {
    #[inline]
    pub const fn stride(self) -> usize {
        self.structural as usize + self.classes + self.exacts
    }

    /// Slot of class 0 within a block.
    #[inline]
    const fn first_class(self) -> usize {
        self.structural as usize
    }
}

/// Reusable storage for the result of one scan.
#[derive(Debug, Clone, Default)]
pub struct StructuralIndex {
    mode: IndexMode,
    layout: MaskLayout,
    input_len: u32,
    tape: Vec<u32>,
    masks: Vec<u64>,
}

impl StructuralIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserve enough space that scanning any input of at most `max_input`
    /// bytes in `mode` will not allocate.
    pub fn try_reserve(
        &mut self,
        mode: IndexMode,
        layout: MaskLayout,
        max_input: usize,
    ) -> Result<(), TryReserveError> {
        if mode != IndexMode::Masks {
            let needed = max_input.saturating_add(SAFETY_PADDING);
            self.tape.clear();
            self.tape.try_reserve_exact(needed)?;
        }
        if mode != IndexMode::Tape {
            let needed = max_input.div_ceil(64).saturating_mul(layout.stride());
            self.masks.clear();
            self.masks.try_reserve_exact(needed)?;
        }
        Ok(())
    }

    /// Clear for a new scan and grow if `input_len` exceeds the reservation.
    pub(crate) fn prepare(&mut self, mode: IndexMode, layout: MaskLayout, input_len: usize) {
        self.mode = mode;
        self.layout = layout;
        self.input_len = input_len as u32;
        self.tape.clear();
        self.masks.clear();
        if mode != IndexMode::Masks {
            self.tape.reserve(input_len + SAFETY_PADDING);
        }
        if mode != IndexMode::Tape {
            self.masks.reserve(input_len.div_ceil(64) * layout.stride());
        }
    }

    /// Append `mask`'s lanes as absolute offsets. Requires the reservation
    /// made by [`Self::prepare`].
    #[inline]
    pub(crate) fn flatten(&mut self, mut mask: u64, base: u32) {
        let tape = &mut self.tape;
        debug_assert!(tape.capacity() - tape.len() >= mask.count_ones() as usize);
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

    #[inline]
    pub(crate) fn push_block(&mut self, structural: u64, masks: &crate::ClassifiedBlock) {
        if self.layout.structural {
            self.masks.push(structural);
        }
        self.masks
            .extend_from_slice(&masks.class[..self.layout.classes]);
        self.masks
            .extend_from_slice(&masks.exact[..self.layout.exacts]);
    }

    #[inline]
    pub fn mode(&self) -> IndexMode {
        self.mode
    }

    #[inline]
    pub fn view(&self) -> IndexView<'_> {
        let masks = MaskView {
            masks: &self.masks,
            layout: self.layout,
            input_len: self.input_len,
        };
        match self.mode {
            IndexMode::Tape => IndexView::Tape(&self.tape),
            IndexMode::Masks => IndexView::Masks(masks),
            IndexMode::Hybrid => IndexView::Hybrid {
                tape: &self.tape,
                masks,
            },
        }
    }

    /// Bytes currently allocated for either representation.
    pub fn capacity_bytes(&self) -> usize {
        self.tape.capacity() * size_of::<u32>() + self.masks.capacity() * size_of::<u64>()
    }
}

/// A borrowed scan result in whichever mode it was built.
#[derive(Debug, Clone, Copy)]
pub enum IndexView<'a> {
    /// Ascending absolute offsets of every structural byte.
    Tape(&'a [u32]),
    /// One group of masks per 64-byte block.
    Masks(MaskView<'a>),
    /// Structural offsets on a tape, plus selected class masks per block.
    Hybrid {
        tape: &'a [u32],
        masks: MaskView<'a>,
    },
}

impl<'a> IndexView<'a> {
    #[inline]
    pub fn mode(self) -> IndexMode {
        match self {
            Self::Tape(_) => IndexMode::Tape,
            Self::Masks(_) => IndexMode::Masks,
            Self::Hybrid { .. } => IndexMode::Hybrid,
        }
    }

    #[inline]
    pub fn tape(self) -> Option<&'a [u32]> {
        match self {
            Self::Tape(tape) | Self::Hybrid { tape, .. } => Some(tape),
            Self::Masks(_) => None,
        }
    }

    #[inline]
    pub fn masks(self) -> Option<MaskView<'a>> {
        match self {
            Self::Tape(_) => None,
            Self::Masks(masks) | Self::Hybrid { masks, .. } => Some(masks),
        }
    }

    /// Structural offsets in ascending order, regardless of mode. In mask
    /// mode offsets are flattened lazily while iterating.
    #[inline]
    pub fn positions(self) -> Positions<'a> {
        match self {
            Self::Tape(tape) | Self::Hybrid { tape, .. } => Positions::Tape(tape.iter()),
            Self::Masks(masks) => Positions::Masks(masks.structural_positions()),
        }
    }

    pub fn structural_count(self) -> usize {
        match self {
            Self::Tape(tape) | Self::Hybrid { tape, .. } => tape.len(),
            Self::Masks(masks) => (0..masks.blocks())
                .map(|block| masks.structural(block).count_ones() as usize)
                .sum(),
        }
    }

    /// Logical size of the index in bytes.
    pub fn bytes(self) -> usize {
        match self {
            Self::Tape(tape) => size_of_val(tape),
            Self::Masks(masks) => size_of_val(masks.masks),
            Self::Hybrid { tape, masks } => size_of_val(tape) + size_of_val(masks.masks),
        }
    }
}

/// Per-block masks produced in [`IndexMode::Masks`] and
/// [`IndexMode::Hybrid`].
#[derive(Debug, Clone, Copy)]
pub struct MaskView<'a> {
    masks: &'a [u64],
    layout: MaskLayout,
    input_len: u32,
}

impl<'a> MaskView<'a> {
    #[inline]
    pub fn blocks(self) -> usize {
        self.masks.len() / self.layout.stride()
    }

    #[inline]
    pub fn layout(self) -> MaskLayout {
        self.layout
    }

    #[inline]
    pub fn input_len(self) -> u32 {
        self.input_len
    }

    /// Structural lanes of `block`, as returned by the spec's `scan_block`.
    /// Only stored in [`IndexMode::Masks`].
    #[inline]
    pub fn structural(self, block: usize) -> u64 {
        assert!(self.layout.structural);
        self.masks[block * self.layout.stride()]
    }

    /// Refined class mask `class` of `block`.
    #[inline]
    pub fn class(self, block: usize, class: usize) -> u64 {
        assert!(class < self.layout.classes);
        self.masks[block * self.layout.stride() + self.layout.first_class() + class]
    }

    /// Refined exact-byte mask `exact` of `block`.
    #[inline]
    pub fn exact(self, block: usize, exact: usize) -> u64 {
        assert!(exact < self.layout.exacts);
        self.masks
            [block * self.layout.stride() + self.layout.first_class() + self.layout.classes + exact]
    }

    /// Offsets of structural lanes, across all blocks.
    #[inline]
    pub fn structural_positions(self) -> MaskLanes<'a> {
        assert!(self.layout.structural);
        self.lanes(0)
    }

    /// Union of the classes selected by `classes` (bit `i` selects class
    /// `i`) in `block`.
    #[inline]
    fn union(self, block: usize, classes: u8) -> u64 {
        let row = &self.masks[block * self.layout.stride() + self.layout.first_class()..]
            [..self.layout.classes];
        let mut union = 0;
        for (class, mask) in row.iter().enumerate() {
            if classes & (1 << class) != 0 {
                union |= mask;
            }
        }
        union
    }

    /// The first offset at or after `from` that belongs to any class
    /// selected by `classes` (bit `i` selects class `i`).
    ///
    /// This is the mask-mode equivalent of advancing along a tape, but the
    /// caller chooses which classes to stop at for each search.
    #[inline]
    pub fn next_in(self, classes: u8, from: usize) -> Option<usize> {
        self.next_where(from, |block| self.union(block, classes))
    }

    /// The first structural offset at or after `from`. Only available in
    /// [`IndexMode::Masks`]; hybrid indexes keep structurals on the tape.
    #[inline]
    pub fn next_structural(self, from: usize) -> Option<usize> {
        assert!(self.layout.structural);
        let stride = self.layout.stride();
        self.next_where(from, |block| self.masks[block * stride])
    }

    #[inline]
    fn next_where(self, from: usize, mask_of: impl Fn(usize) -> u64) -> Option<usize> {
        let blocks = self.blocks();
        let mut block = from / 64;
        if block >= blocks {
            return None;
        }
        let mut mask = mask_of(block) & (u64::MAX << (from % 64));
        loop {
            if mask != 0 {
                return Some(block * 64 + mask.trailing_zeros() as usize);
            }
            block += 1;
            if block >= blocks {
                return None;
            }
            mask = mask_of(block);
        }
    }

    /// The number of consecutive offsets, starting at `from`, that belong to
    /// class `class`. With a space class this measures indentation in one
    /// or two mask reads instead of a byte loop.
    #[inline]
    pub fn run_length(self, class: usize, from: usize) -> usize {
        assert!(class < self.layout.classes);
        let blocks = self.blocks();
        let mut block = from / 64;
        let mut lane = from % 64;
        let mut run = 0;
        while block < blocks {
            let mask = self.masks[block * self.layout.stride() + self.layout.first_class() + class]
                >> lane;
            let ones = mask.trailing_ones() as usize;
            run += ones;
            if ones < 64 - lane {
                break;
            }
            block += 1;
            lane = 0;
        }
        run
    }

    /// Offsets of the set lanes of class `class`, across all blocks.
    #[inline]
    pub fn class_positions(self, class: usize) -> MaskLanes<'a> {
        assert!(class < self.layout.classes);
        self.lanes(self.layout.first_class() + class)
    }

    #[inline]
    fn lanes(self, slot: usize) -> MaskLanes<'a> {
        MaskLanes {
            masks: self.masks,
            stride: self.layout.stride(),
            slot,
            next_block: 0,
            base: 0,
            current: 0,
        }
    }
}

/// Lazily flattened offsets of one mask slot.
#[derive(Debug, Clone)]
pub struct MaskLanes<'a> {
    masks: &'a [u64],
    stride: usize,
    slot: usize,
    next_block: usize,
    base: u32,
    current: u64,
}

impl Iterator for MaskLanes<'_> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        while self.current == 0 {
            let index = self.next_block * self.stride + self.slot;
            self.current = *self.masks.get(index)?;
            self.base = (self.next_block * 64) as u32;
            self.next_block += 1;
        }
        let lane = self.current.trailing_zeros();
        self.current &= self.current - 1;
        Some(self.base + lane)
    }
}

/// Structural offsets from either representation.
#[derive(Debug, Clone)]
pub enum Positions<'a> {
    Tape(std::slice::Iter<'a, u32>),
    Masks(MaskLanes<'a>),
}

impl Iterator for Positions<'_> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        match self {
            Self::Tape(tape) => tape.next().copied(),
            Self::Masks(lanes) => lanes.next(),
        }
    }
}
