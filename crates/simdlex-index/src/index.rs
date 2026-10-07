use std::collections::TryReserveError;
use std::ops::Range;

use crate::ClassifiedBlock;

/// Extra tape slots reserved past the window length so flattening may write
/// without per-offset bounds checks.
pub const SAFETY_PADDING: usize = 8;

/// The result of indexing one window of the input: a tape of structural
/// offsets and, for specs with mask classes, one mask per class per block.
/// Reuse it across windows and documents to avoid allocation.
#[derive(Debug, Clone, Default)]
pub struct StructuralIndex {
    window: Range<usize>,
    mask_classes: usize,
    tape: Vec<u32>,
    masks: Vec<u64>,
}

impl StructuralIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserve enough space that indexing any window of at most `max_window`
    /// bytes with `mask_classes` mask classes will not allocate.
    pub fn try_reserve(
        &mut self,
        mask_classes: usize,
        max_window: usize,
    ) -> Result<(), TryReserveError> {
        self.tape.clear();
        self.tape
            .try_reserve_exact(max_window.saturating_add(SAFETY_PADDING))?;
        self.masks.clear();
        self.masks
            .try_reserve_exact(max_window.div_ceil(64).saturating_mul(mask_classes))
    }

    /// Clear for a new window and grow if it exceeds the reservation.
    pub(crate) fn prepare(&mut self, window: Range<usize>, mask_classes: usize) {
        let len = window.len();
        self.window = window;
        self.mask_classes = mask_classes;
        self.tape.clear();
        self.masks.clear();
        self.tape.reserve(len + SAFETY_PADDING);
        self.masks.reserve(len.div_ceil(64) * mask_classes);
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
    pub(crate) fn push_masks(&mut self, masks: &ClassifiedBlock) {
        self.masks
            .extend_from_slice(&masks.class[..self.mask_classes]);
    }

    /// The input range this index covers.
    #[inline]
    pub fn window(&self) -> Range<usize> {
        self.window.clone()
    }

    /// Ascending absolute offsets of every structural byte in the window.
    #[inline]
    pub fn tape(&self) -> &[u32] {
        &self.tape
    }

    #[inline]
    pub fn cursor(&self) -> TapeCursor<'_> {
        TapeCursor::new(&self.tape)
    }

    /// Per-block masks of the spec's mask classes.
    #[inline]
    pub fn masks(&self) -> MaskView<'_> {
        MaskView {
            masks: &self.masks,
            classes: self.mask_classes,
            first_block: self.window.start / 64,
            end: self.window.end,
        }
    }

    /// Logical size of the tape and masks in bytes.
    pub fn bytes(&self) -> usize {
        size_of_val(self.tape.as_slice()) + size_of_val(self.masks.as_slice())
    }

    /// Bytes currently allocated.
    pub fn capacity_bytes(&self) -> usize {
        self.tape.capacity() * size_of::<u32>() + self.masks.capacity() * size_of::<u64>()
    }
}

/// A forward-moving position on a tape.
///
/// Stage 2 mostly asks "where is the next structural of this kind at or
/// after here?". [`Self::find`] answers that and leaves the cursor on the
/// match, so asking again from at or before it returns it again. [`Self::seek`]
/// moves to any offset, galloping forward from the current position before
/// falling back to binary search, so callers whose own position jumps (after
/// scanning a token without the tape) stay cheap.
#[derive(Debug, Clone, Copy)]
pub struct TapeCursor<'a> {
    tape: &'a [u32],
    next: usize,
}

impl<'a> TapeCursor<'a> {
    #[inline]
    pub fn new(tape: &'a [u32]) -> Self {
        Self { tape, next: 0 }
    }

    /// The tape this cursor walks.
    #[inline]
    pub fn tape(&self) -> &'a [u32] {
        self.tape
    }

    /// Index of the current entry on the tape.
    #[inline]
    pub fn index(&self) -> usize {
        self.next
    }

    /// The current entry's offset, if any remain.
    #[inline]
    pub fn peek(&self) -> Option<usize> {
        self.tape.get(self.next).map(|&offset| offset as usize)
    }

    /// Step past the current entry.
    #[inline]
    pub fn advance(&mut self) {
        self.next += 1;
    }

    /// Move to the first entry at or after `offset`.
    #[inline]
    pub fn seek(&mut self, offset: usize) {
        let tape = self.tape;
        let before = |index: usize| (tape[index] as usize) < offset;
        if self.next > tape.len() || (self.next > 0 && !before(self.next - 1)) {
            self.next = tape.partition_point(|&entry| (entry as usize) < offset);
            return;
        }
        for _ in 0..8 {
            if self.next >= tape.len() || !before(self.next) {
                return;
            }
            self.next += 1;
        }
        self.next += tape[self.next..].partition_point(|&entry| (entry as usize) < offset);
    }

    /// The first entry at or after `from` whose byte satisfies `wanted`.
    /// The cursor is left on the match, or at the end if there is none.
    #[inline]
    pub fn find(
        &mut self,
        input: &[u8],
        from: usize,
        wanted: impl Fn(u8) -> bool,
    ) -> Option<usize> {
        self.find_before(input, from, usize::MAX, wanted)
    }

    /// Like [`Self::find`], but gives up at the first entry at or after
    /// `limit`, leaving the cursor there.
    #[inline]
    pub fn find_before(
        &mut self,
        input: &[u8],
        from: usize,
        limit: usize,
        wanted: impl Fn(u8) -> bool,
    ) -> Option<usize> {
        self.seek(from);
        while let Some(offset) = self.peek() {
            if offset >= limit {
                return None;
            }
            if wanted(input[offset]) {
                return Some(offset);
            }
            self.next += 1;
        }
        None
    }
}

impl Iterator for TapeCursor<'_> {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        let offset = self.peek()?;
        self.next += 1;
        Some(offset)
    }
}

/// Per-block masks of a spec's mask classes. Offsets are absolute; only the
/// indexed window has masks.
#[derive(Debug, Clone, Copy)]
pub struct MaskView<'a> {
    masks: &'a [u64],
    classes: usize,
    first_block: usize,
    end: usize,
}

impl<'a> MaskView<'a> {
    /// Number of mask classes per block.
    #[inline]
    pub fn classes(self) -> usize {
        self.classes
    }

    /// Absolute block range covered.
    #[inline]
    pub fn blocks(self) -> Range<usize> {
        let count = self.masks.len().checked_div(self.classes).unwrap_or(0);
        self.first_block..self.first_block + count
    }

    /// Mask of class `class` in absolute block `block`.
    #[inline]
    pub fn class(self, block: usize, class: usize) -> u64 {
        assert!(class < self.classes);
        self.masks[(block - self.first_block) * self.classes + class]
    }

    /// Union of the classes selected by `classes` (bit `i` selects class
    /// `i`) in relative block `relative`.
    #[inline]
    fn union(self, relative: usize, classes: u8) -> u64 {
        let row = &self.masks[relative * self.classes..][..self.classes];
        let mut union = 0;
        for (class, mask) in row.iter().enumerate() {
            if classes & (1 << class) != 0 {
                union |= mask;
            }
        }
        union
    }

    /// The first offset at or after `from` that belongs to any class
    /// selected by `classes` (bit `i` selects class `i`). This is the mask
    /// equivalent of walking a tape, with the classes chosen per search.
    #[inline]
    pub fn next_in(self, classes: u8, from: usize) -> Option<usize> {
        let blocks = self.blocks().len();
        let from = from.max(self.first_block * 64);
        let mut relative = from / 64 - self.first_block;
        if relative >= blocks {
            return None;
        }
        let mut mask = self.union(relative, classes) & (u64::MAX << (from % 64));
        loop {
            if mask != 0 {
                let offset = (self.first_block + relative) * 64 + mask.trailing_zeros() as usize;
                return (offset < self.end).then_some(offset);
            }
            relative += 1;
            if relative >= blocks {
                return None;
            }
            mask = self.union(relative, classes);
        }
    }

    /// The number of consecutive offsets, starting at `from`, that belong to
    /// class `class`. With a space class this measures indentation in one or
    /// two mask reads instead of a byte loop.
    #[inline]
    pub fn run_length(self, class: usize, from: usize) -> usize {
        assert!(class < self.classes);
        let blocks = self.blocks();
        let mut block = from / 64;
        let mut lane = from % 64;
        let mut run = 0;
        while blocks.contains(&block) {
            let mask = self.class(block, class) >> lane;
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

    /// Offsets of the set lanes of class `class`.
    #[inline]
    pub fn class_positions(self, class: usize) -> impl Iterator<Item = usize> + 'a {
        assert!(class < self.classes);
        let first_block = self.first_block;
        self.masks
            .chunks_exact(self.classes)
            .enumerate()
            .flat_map(move |(relative, row)| {
                let base = (first_block + relative) * 64;
                let mut mask = row[class];
                std::iter::from_fn(move || {
                    if mask == 0 {
                        return None;
                    }
                    let lane = mask.trailing_zeros() as usize;
                    mask &= mask - 1;
                    Some(base + lane)
                })
            })
    }
}
